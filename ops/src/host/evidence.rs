//! Commands whose output is evidence. Every command of one attempt leaves a
//! numbered private pair of files in the attempt's directory, and nothing a
//! command says is retained beyond a ceiling fixed before it starts.

use super::command::Request;
use super::failure::{Failure, Result, require};
use super::{DOCKER, Host};
use std::path::{Path, PathBuf};

/// The environment of every command that leaves evidence.
pub(crate) const ENVIRONMENT: [(&str, &str); 2] =
    [("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"), ("LC_ALL", "C")];
const MAX_INSPECTION_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const ARCHIVE_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;

/// What a kept command reads, and where its standard output goes when that
/// is not the numbered file.
#[derive(Clone, Copy, Default)]
pub(crate) struct Streams<'a> {
    /// Bytes given as standard input.
    pub input: Option<&'a [u8]>,
    /// A file given as standard input.
    pub source: Option<&'a Path>,
    /// A new private archive that receives standard output.
    pub archive: Option<&'a Path>,
    /// The caller's own ceiling for `archive`.
    pub limit: Option<u64>,
}

/// The commands of one attempt.
pub(crate) struct Evidence<'a> {
    pub host: &'a Host<'a>,
    pub attempt: PathBuf,
    /// How many commands this attempt has issued.
    pub number: u32,
}

impl<'a> Evidence<'a> {
    pub(crate) fn new(host: &'a Host<'a>, attempt: PathBuf) -> Self {
        let number = 0;
        Self {
            host,
            attempt,
            number,
        }
    }

    /// The most an archive at `path` may grow to: the caller's limit or
    /// 4 GiB, and never into the last gibibyte of its filesystem.
    fn budget(&self, path: &Path, selected: Option<u64>) -> Result<u64> {
        let requested = selected.unwrap_or(MAX_ARCHIVE_BYTES);
        let valid = 0 < requested && requested <= MAX_ARCHIVE_BYTES;
        require(valid, "Invalid archive output limit")?;
        let parent = path.parent().ok_or(Failure::Os)?;
        let free = self.host.commands.free(parent)?;
        let maximum = requested.min(free.saturating_sub(ARCHIVE_RESERVE_BYTES));
        require(maximum > 0, "Insufficient archive output reserve")?;
        Ok(maximum)
    }

    /// Run one command to its deadline and return its inspected output, or
    /// nothing when the output went to an archive.
    pub(crate) fn run(
        &mut self,
        argv: Vec<String>,
        seconds: f64,
        streams: Streams<'_>,
    ) -> Result<Vec<u8>> {
        self.number += 1;
        let prefix = self.attempt.join(format!("{:03}", self.number));
        let (kept, errors) = (
            prefix.with_extension("stdout"),
            prefix.with_extension("stderr"),
        );
        let selected = streams.archive.is_some() || streams.limit.is_none();
        require(selected, "Output limits require an archive path")?;
        let ceiling = match streams.archive {
            Some(path) => self.budget(path, streams.limit)?,
            None => MAX_INSPECTION_BYTES,
        };
        let request = Request {
            environment: &ENVIRONMENT,
            input: streams.input,
            source: streams.source,
            archive: Some(streams.archive.unwrap_or(&kept)),
            ceiling: Some(ceiling),
            errors: Some(&errors),
            ..Request::fixed(
                argv,
                seconds,
                0,
                "Command inspection output exceeded its bound",
            )
        };
        self.host.commands.run(&request)?;
        if streams.archive.is_some() {
            return Ok(Vec::new());
        }
        let bounded = std::fs::metadata(&kept)?.len() <= MAX_INSPECTION_BYTES;
        require(bounded, "Command inspection output exceeded its bound")?;
        Ok(std::fs::read(&kept)?)
    }

    /// Run the local daemon's client with fixed arguments.
    pub(crate) fn docker(
        &mut self,
        arguments: &[&str],
        seconds: f64,
        streams: Streams<'_>,
    ) -> Result<Vec<u8>> {
        let argv = DOCKER
            .iter()
            .chain(arguments)
            .map(|word| (*word).to_owned());
        self.run(argv.collect(), seconds, streams)
    }
}

#[cfg(test)]
mod tests {
    use super::super::command::tests::Scripted;
    use super::super::state::tests::Scratch;
    use super::super::tests::{Fixed, paths};
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// Inspected output is read back from its numbered file under the fixed
    /// environment; an archive gets the smaller of its limit and the room
    /// above the reserve, and a limit without an archive, a limit beyond the
    /// general one or a filesystem inside its reserve issues no command.
    #[test]
    fn every_command_is_numbered_bounded_and_refused_before_it_can_overrun() {
        let scratch = Scratch::new();
        let commands = Scripted::new(|request: &Request<'_>| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            std::fs::write(request.archive.unwrap(), b"said").unwrap();
            options.open(request.errors.unwrap()).unwrap();
            Ok(Vec::new())
        });
        let clock = Fixed::at("2026-11-11T04:00:07Z");
        let host = Host {
            commands: &commands,
            clock: &clock,
            paths: paths(scratch.path()),
            owner: 0,
        };
        let mut evidence = Evidence::new(&host, scratch.path().to_owned());
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let plain = Streams::default();
        assert_eq!(
            evidence.run(words("/bin/echo one"), 3.0, plain).unwrap(),
            b"said"
        );
        assert_eq!(
            evidence.docker(&["ps", "--quiet"], 30.0, plain).unwrap(),
            b"said"
        );
        let archive = scratch.path().join("database.dump");
        let to_archive = |limit| Streams {
            input: Some(b"typed"),
            archive: Some(&archive),
            limit,
            ..Streams::default()
        };
        commands.free.set(Some(3 * GIB));
        assert_eq!(
            evidence
                .run(words("/bin/dump"), 130.0, to_archive(None))
                .unwrap(),
            b""
        );
        std::fs::remove_file(&archive).unwrap();
        evidence
            .run(words("/bin/dump"), 9.0, to_archive(Some(4096)))
            .unwrap();
        commands.free.set(Some(GIB + 7));
        std::fs::remove_file(&archive).unwrap();
        evidence
            .run(words("/bin/dump"), 9.0, to_archive(Some(4096)))
            .unwrap();
        {
            let asked = commands.asked.borrow();
            let ceilings: Vec<_> = asked.iter().map(|asked| asked.ceiling.unwrap()).collect();
            assert_eq!(
                ceilings,
                [2 * 1024 * 1024, 2 * 1024 * 1024, 2 * GIB, 4096, 7]
            );
            let docker = "/usr/bin/docker --host unix:///var/run/docker.sock ps --quiet";
            assert_eq!(asked[1].argv.join(" "), docker);
            for (number, command) in asked.iter().enumerate() {
                let kept = scratch.path().join(format!("{:03}.stdout", number + 1));
                let expected = if number < 2 { kept } else { archive.clone() };
                assert_eq!(command.archive.as_deref(), Some(expected.as_path()));
                let pairs = ENVIRONMENT.map(|(key, value)| (key.to_owned(), value.to_owned()));
                assert_eq!(command.environment, pairs);
                assert!(
                    scratch
                        .path()
                        .join(format!("{:03}.stderr", number + 1))
                        .exists()
                );
            }
            assert_eq!(asked[2].input.as_deref(), Some(&b"typed"[..]));
            assert!((asked[2].seconds - 130.0).abs() < f64::EPSILON);
        }
        commands.free.set(Some(GIB));
        let reserve = Err(Failure::Refused("Insufficient archive output reserve"));
        assert_eq!(
            evidence.run(words("/bin/dump"), 9.0, to_archive(None)),
            reserve
        );
        let invalid = Err(Failure::Refused("Invalid archive output limit"));
        for limit in [0, 4 * GIB + 1] {
            assert_eq!(
                evidence.run(words("/bin/dump"), 9.0, to_archive(Some(limit))),
                invalid
            );
        }
        let stray = Streams {
            limit: Some(1),
            ..Streams::default()
        };
        let unselected = Err(Failure::Refused("Output limits require an archive path"));
        assert_eq!(evidence.run(words("/bin/echo"), 9.0, stray), unselected);
        // Refused commands were counted but never issued.
        assert_eq!((evidence.number, commands.asked.borrow().len()), (9, 5));
    }
}
