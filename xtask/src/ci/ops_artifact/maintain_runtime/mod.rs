//! Rust outer keeper for the native guest and frozen installation comparisons.

mod auth;
mod catalog;
mod control;
mod execute;
mod files;
mod host_projection;
mod options;
mod process;
mod projection;
mod source;
#[cfg(test)]
mod tests;
mod vm;
mod work;

/// Synchronous outer owner survives runtime teardown, mask restoration and output.
pub(super) fn run(arguments: &[String]) -> std::process::ExitCode {
    run_for(arguments, options::Purpose::Prerequisite)
}
pub(super) fn run_host(arguments: &[String]) -> std::process::ExitCode {
    run_for(arguments, options::Purpose::Host)
}
fn run_for(arguments: &[String], purpose: options::Purpose) -> std::process::ExitCode {
    use std::{io::Write, time::Instant};
    let started = Instant::now();
    let parsed = match purpose {
        options::Purpose::Prerequisite => options::Options::parse(arguments),
        options::Purpose::Host => options::Options::parse_host(arguments),
    };
    let options = match parsed {
        Ok(None) => {
            println!("{}", purpose.usage());
            return std::process::ExitCode::SUCCESS;
        }
        Ok(Some(value)) => value,
        Err(_) => {
            eprintln!("{}", purpose.usage());
            return std::process::ExitCode::from(2);
        }
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let mut control = match control::Control::for_purpose(started, purpose) {
        Ok(value) => value,
        Err(_) => {
            let _ = nix::sys::stat::umask(previous);
            return std::process::ExitCode::FAILURE;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build();
    let mut report = match runtime {
        Ok(runtime) => {
            let report = runtime.block_on(execute::run(&options, &mut control));
            drop(runtime);
            report
        }
        Err(_) => {
            control.record(Error::Io);
            control.settle();
            serde_json::json!({"schemaVersion":2,
                "scope":purpose.summary_scope(),
                "scenario":options.scenario,"passed":false,"failure":"runtime",
                "failureKind":"io","cleanupPassed":false,"preparationQemu":null})
        }
    };
    let _ = nix::sys::stat::umask(previous);
    if let Err(error) = control.check() {
        control.record(error);
    }
    if let Some(error) = control.first {
        report["passed"] = false.into();
        report["failureKind"] = error.kind().into();
        if report["failure"].is_null() {
            report["failure"] = "finalization".into();
        }
    }
    let published = (|| {
        let bytes = auth::encoded(&report)?;
        if bytes.len() > 131072 {
            return Err(Error::Refused);
        }
        if options.output.is_dir() {
            files::write(&options.output.join("summary.json"), &bytes, &mut control)?;
        }
        control.check()?;
        let mut output = std::io::stdout().lock();
        output.write_all(&bytes).map_err(|_| Error::Io)?;
        output.flush().map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(error) = published {
        control.record(error);
    }
    let finished = control.finish();
    if finished.is_ok() && report["passed"] == true {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(outward_status(control.status, false))
    }
}
fn outward_status(observed_signal: u8, passed: bool) -> u8 {
    if [130, 143].contains(&observed_signal) {
        observed_signal
    } else if passed {
        0
    } else {
        1
    }
}

/// The fixed wire never admits duplicate object keys or a second JSON value.
pub(super) fn decode(bytes: &[u8]) -> Result<serde_json::Value> {
    use serde::Deserialize;
    struct Unique(serde_json::Value);
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Unique;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("one finite unique-key JSON value")
        }
        fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
            Ok(Unique(v.into()))
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
            Ok(Unique(v.into()))
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
            Ok(Unique(v.into()))
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
            serde_json::Number::from_f64(v)
                .map(|v| Unique(v.into()))
                .ok_or_else(|| E::custom("nonfinite JSON"))
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
            Ok(Unique(v.into()))
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
            Ok(Unique(serde_json::Value::Null))
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Unique, A::Error> {
            let mut values = Vec::new();
            while let Some(Unique(value)) = seq.next_element()? {
                values.push(value);
            }
            Ok(Unique(values.into()))
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Unique, A::Error> {
            let mut values = serde_json::Map::new();
            while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate JSON key"));
                }
            }
            Ok(Unique(values.into()))
        }
    }
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(
            decoder: D,
        ) -> std::result::Result<Self, D::Error> {
            decoder.deserialize_any(Visitor)
        }
    }
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(Error::Refused);
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let Unique(value) = Unique::deserialize(&mut decoder).map_err(|_| Error::Input)?;
    decoder.end().map_err(|_| Error::Input)?;
    Ok(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Error {
    Input,
    Refused,
    Io,
    Changed,
    Close,
    Deadline,
    Interrupt,
    Terminate,
    Child(i32),
}
pub(super) type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Io | Self::Close => "io",
            Self::Deadline => "deadline",
            Self::Interrupt | Self::Terminate => "signal",
            _ => "refused",
        }
    }
}
impl From<crate::tooling::Failure> for Error {
    fn from(error: crate::tooling::Failure) -> Self {
        match error.code {
            124 => Self::Deadline,
            130 => Self::Interrupt,
            143 => Self::Terminate,
            _ => Self::Refused,
        }
    }
}
