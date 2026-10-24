//! Parse the three documented selectors before any operation effect.

use super::control::{Failure, Result};
use std::{ffi::OsString, path::PathBuf};

pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release build --output PATH [--timeout-seconds N] ",
    "[--image-id sha256:HEX]\n",
    "       simplestchat-ops release build --licenses\n",
    "Build or export an unsigned Linux/amd64 release from this clean checkout.\n",
    "N is 60..7200 (default 3600). The active window starts after source setup.\n",
    "Whole operation: build N+300 seconds; export N+60 seconds, including cleanup.\n",
    "Export requires an exact tested local image and never builds or pulls.\n",
    "Output must be fresh, outside this checkout or inside an ignored directory."
);

#[derive(Debug)]
pub(super) struct Options {
    pub output: PathBuf,
    pub seconds: u64,
    pub image: Option<String>,
}
impl Options {
    pub fn parse(arguments: &[OsString]) -> Result<Option<Self>> {
        if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
            return Ok(None);
        }
        let mut output = None;
        let mut seconds = None;
        let mut image = None;
        let mut index = 0;
        while index < arguments.len() {
            let argument = arguments[index].to_str().ok_or(Failure::Input)?;
            let (name, equal) = argument
                .split_once('=')
                .map_or((argument, None), |(name, value)| (name, Some(value)));
            let value = if let Some(value) = equal {
                OsString::from(value)
            } else {
                index += 1;
                arguments.get(index).ok_or(Failure::Input)?.clone()
            };
            match name {
                "--output" if output.is_none() && !value.is_empty() => {
                    output = Some(PathBuf::from(value));
                }
                "--timeout-seconds" if seconds.is_none() => {
                    let value = value.to_str().ok_or(Failure::Input)?;
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(Failure::Input);
                    }
                    seconds = Some(value.parse::<u64>().map_err(|_| Failure::Input)?);
                }
                "--image-id" if image.is_none() => {
                    let value = value.into_string().map_err(|_| Failure::Input)?;
                    if !value
                        .strip_prefix("sha256:")
                        .is_some_and(|value| hex(value, 64))
                    {
                        return Err(Failure::Input);
                    }
                    image = Some(value);
                }
                _ => return Err(Failure::Input),
            }
            index += 1;
        }
        let seconds = seconds.unwrap_or(3600);
        if !(60..=7200).contains(&seconds) {
            return Err(Failure::Input);
        }
        Ok(Some(Self {
            output: output.ok_or(Failure::Input)?,
            seconds,
            image,
        }))
    }
}
pub(super) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    #[test]
    fn help_equals_duplicates_and_numeric_domain() {
        assert!(Options::parse(&args(&["--help"])).unwrap().is_none());
        let value = Options::parse(&args(&["--output=/tmp/new", "--timeout-seconds=060"]))
            .unwrap()
            .unwrap();
        assert_eq!(value.seconds, 60);
        for values in [
            vec!["--output", "a", "--output=b"],
            vec!["--output", "a", "--timeout-seconds", "59"],
            vec!["--output", "a", "--timeout-seconds", "7201"],
            vec!["--output", "a", "--timeout-seconds", "+60"],
            vec!["--out", "a"],
            vec!["--output", "a", "--help"],
        ] {
            assert!(Options::parse(&args(&values)).is_err());
        }
    }
}
