//! Fixed bootstrap entry with exact input framing and final output fences.

use super::{
    Failure, Result,
    control::Owner,
    files::{self, Access, Fs},
    host::{Session, Stage},
    process::{self, Input, Runner},
    protocol::{self, Action, Envelope, Header, Mode, ResultData, Unit},
};
use std::{ffi::OsStr, process::ExitCode};

fn origin(value: &OsStr) -> Result<u64> {
    let value = value.to_str().ok_or(Failure::Arguments)?;
    let parsed = value.parse::<u64>().map_err(|_| Failure::Arguments)?;
    if parsed == 0 || parsed.to_string() != value {
        return Err(Failure::Arguments);
    }
    Ok(parsed)
}

fn reserve(session: &mut Session<'_>, input: &mut Input) -> Result<()> {
    let lock = session.lock()?;
    let held = (|| {
        session.preflight(None)?;
        let ready = protocol::Response::encode(
            session.header,
            &ResultData::Reserve(protocol::Ready {
                private_host: true,
                ready: true,
            }),
        )?;
        process::ready(&ready, session.owner)?;
        let release_start = session.owner.fixed_end(1740)?;
        let first = input.exact(1, session.owner, release_start)?;
        let end = session.owner.begin_release()?;
        let rest = input.exact(7, session.owner, end)?;
        if first != b"r" || rest != b"elease\n" {
            return Err(Failure::Framing);
        }
        input.eof(session.owner, end)
    })();
    let unlocked = session.unlock(lock);
    held.and(unlocked)
}

fn execute(header: Header, origin: u64) -> Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(Failure::Platform);
    }
    let uname = rustix::system::uname();
    if uname.sysname() != c"Linux" || uname.machine() != c"x86_64" {
        return Err(Failure::Platform);
    }
    let mut owner = Owner::new(origin, header.mode)?;
    let mut fs = match Fs::new(Access::default()) {
        Ok(fs) => fs,
        Err(error) => {
            let _ = owner.finish();
            return Err(error);
        }
    };
    let mut input = match Input::new() {
        Ok(input) => input,
        Err(error) => {
            let _ = fs.finish();
            let _ = owner.finish();
            return Err(error);
        }
    };
    let mut runner = Runner::new();
    let body = (|| {
        let end = owner.active_end();
        let bytes = input.exact(
            usize::try_from(header.request.bytes).map_err(|_| Failure::Bounds)?,
            &mut owner,
            end,
        )?;
        if files::sha(&bytes) != header.request.sha256 {
            return Err(Failure::Identity);
        }
        let envelope = Envelope::decode(&bytes)?;
        envelope.bind(&header)?;
        if envelope.action() != Action::Reserve {
            input.eof(&mut owner, end)?;
        }
        owner.admit(envelope.action())?;
        let stage = Stage::open(&header, &mut fs, &mut owner)?;
        if header.mode == Mode::Stage {
            if !matches!(envelope.action(), Action::Preflight | Action::Lookup) {
                return Err(Failure::Request);
            }
            let exists = fs.exists(&header.evidence_path())?;
            if (envelope.action() == Action::Preflight && exists)
                || (envelope.action() == Action::Lookup && !exists)
            {
                return Err(Failure::Identity);
            }
        }
        let workload = if envelope.action() == Action::Lookup {
            None
        } else {
            Some(envelope.workload()?)
        };
        let mut session = Session {
            header: &header,
            fs: &mut fs,
            owner: &mut owner,
            runner: &mut runner,
        };
        let action = envelope.action();
        let result = match action {
            Action::Reserve => {
                reserve(&mut session, &mut input)?;
                None
            }
            Action::Archive => {
                session.archive(workload.as_ref().ok_or(Failure::Request)?)?;
                None
            }
            Action::Retire => {
                Some(session.retire(workload.as_ref().ok_or(Failure::Request)?, &stage)?)
            }
            Action::Status => {
                let selected = envelope.unit().ok_or(Failure::Request)?;
                if selected == Unit::Capacity {
                    session.require_saved(workload.as_ref().ok_or(Failure::Request)?)?;
                }
                Some(ResultData::Status(session.unit(selected, false)?))
            }
            _ => Some(session.dispatch(action, workload.as_ref(), &stage)?),
        };
        if action != Action::Retire {
            stage.check(session.fs, session.owner)?;
        }
        session.owner.check(false)?;
        result
            .map(|result| protocol::Response::encode(&header, &result))
            .transpose()
    })();
    if let Err(error) = &body {
        owner.latch(*error);
        owner.begin_cleanup();
    }
    let primary = owner.failure().or_else(|| body.as_ref().err().copied());
    let input_closed = input.finish();
    let files_closed = fs.finish();
    if primary.is_none()
        && let Err(error) = input_closed.as_ref().and(files_closed.as_ref())
    {
        owner.latch(*error);
    }
    let finished = owner.finish_fenced();
    if let Some(error) = primary {
        return Err(error);
    }
    input_closed?;
    files_closed?;
    let mut fence = finished?;
    if let Some(bytes) = body? {
        process::emit(&bytes, &mut fence)?;
    }
    fence.check()
}

pub(super) fn run(header: &OsStr, original_origin: &OsStr) -> ExitCode {
    let result = (|| {
        let origin = origin(original_origin)?;
        let header = header.to_str().ok_or(Failure::Arguments)?;
        execute(Header::decode(header.as_bytes())?, origin)
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            process::failure(error);
            ExitCode::from(error.exit_code())
        }
    }
}
