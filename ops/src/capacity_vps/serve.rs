//! Fixed invocation entry with exact input framing and final output fences.

use super::{
    Failure, Result,
    control::{self, Owner},
    files::{self, Access, Fs},
    host::{Installed, Session},
    process::{self, Input, Runner},
    protocol::{self, Action, Envelope, Header, ResultData, Unit},
};
use std::{ffi::OsStr, process::ExitCode};

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
    let mut owner = Owner::new(origin)?;
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
        let installed = Installed::open(&header, &mut fs, &mut owner)?;
        // A new run's first contact finds no evidence; an adopted run's has it.
        let exists = fs.exists(&header.evidence_path())?;
        if (envelope.action() == Action::Preflight && exists)
            || (envelope.action() == Action::Lookup && !exists)
        {
            return Err(Failure::Identity);
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
            Action::Conclude => Some(session.conclude(workload.as_ref().ok_or(Failure::Request)?)?),
            Action::Status => {
                let selected = envelope.unit().ok_or(Failure::Request)?;
                if selected == Unit::Capacity {
                    session.require_saved(workload.as_ref().ok_or(Failure::Request)?)?;
                }
                Some(ResultData::Status(session.unit(selected, false)?))
            }
            _ => Some(session.dispatch(action, workload.as_ref(), &installed)?),
        };
        installed.check(session.fs, session.owner)?;
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

/// Sample the origin of every deadline before anything else, then serve.
pub(super) fn run(header: &OsStr) -> ExitCode {
    let origin = control::now();
    let result = (|| {
        let origin = origin?;
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
