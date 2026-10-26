//! Safe registered atomic flags survive runtime and umask restoration.
//! Registration happens before this operation creates runtime worker threads.

use signal_hook::{
    SigId,
    consts::signal::{SIGINT, SIGTERM},
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) struct Signals {
    interrupt: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
    registrations: [Option<SigId>; 2],
    seen_interrupt: bool,
    seen_terminate: bool,
}
impl Signals {
    /// Observe sticky flags without consuming another caller's observation.
    pub fn status(&self) -> Option<u8> {
        if self.interrupt.load(Ordering::SeqCst) {
            Some(130)
        } else if self.terminate.load(Ordering::SeqCst) {
            Some(143)
        } else {
            None
        }
    }
    pub fn new() -> io::Result<Self> {
        let mut signals = Self {
            interrupt: Arc::new(AtomicBool::new(false)),
            terminate: Arc::new(AtomicBool::new(false)),
            registrations: [None, None],
            seen_interrupt: false,
            seen_terminate: false,
        };
        signals.registrations[0] = Some(signal_hook::flag::register(
            SIGINT,
            Arc::clone(&signals.interrupt),
        )?);
        match signal_hook::flag::register(SIGTERM, Arc::clone(&signals.terminate)) {
            Ok(id) => signals.registrations[1] = Some(id),
            Err(first) => {
                let _ = signals.unregister();
                return Err(first);
            }
        }
        Ok(signals)
    }
    /// Match the existing INT-before-TERM sampling when both are pending.
    pub fn pending_now(&mut self) -> Option<u8> {
        if self.interrupt.load(Ordering::SeqCst) && !self.seen_interrupt {
            self.seen_interrupt = true;
            Some(130)
        } else if self.terminate.load(Ordering::SeqCst) && !self.seen_terminate {
            self.seen_terminate = true;
            Some(143)
        } else {
            None
        }
    }
    /// Attempt every original registration; retain flags for the final fence.
    pub fn unregister(&mut self) -> bool {
        let mut complete = true;
        for registration in &mut self.registrations {
            if let Some(id) = registration.take() {
                complete &= signal_hook::low_level::unregister(id);
            }
        }
        complete
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        // Refusal/unwind cleanup only; positive completion calls unregister and
        // checks its result explicitly before the retained flags' final sample.
        let _ = self.unregister();
    }
}

/// Exact upstream MIT notice for this newly added safe adapter dependency.
pub(in crate::release::attestation) fn notice(mut writer: impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"signal-hook 0.4.5 - MIT\n\n")?;
    writer.write_all(LICENSE.as_bytes())?;
    writer.write_all(b"\n")
}
const LICENSE: &str = r#"Copyright (c) 2017 tokio-jsonrpc developers

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn authenticated_upstream_mit_bytes_remain_exact() {
        assert_eq!(LICENSE.len(), 1068);
        assert_eq!(
            crate::release::attestation::files::hex(&Sha256::digest(LICENSE.as_bytes())),
            "503558bfefe66ca15e4e3f7955b3cb0ec87fd52f29bf24b336af7bd00e946d5c"
        );
    }
    #[test]
    fn safe_adapter_keeps_inferred_thread_traits_without_unsafe_implementations() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Signals>();
        send_sync::<super::super::Owner>();
    }
    #[test]
    fn first_pending_observation_keeps_the_atomic_latch_for_completion() {
        let mut signals = Signals::new().unwrap();
        signals.interrupt.store(true, Ordering::SeqCst);
        let first = signals.pending_now();
        let second = signals.pending_now();
        let retained = signals.interrupt.load(Ordering::SeqCst);
        let closed = signals.unregister();
        assert_eq!(first, Some(130));
        assert_eq!(second, None);
        assert!(retained && closed);
        assert!(signals.registrations.iter().all(Option::is_none));
    }
}
