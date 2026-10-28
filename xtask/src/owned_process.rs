//! Compatibility edge for the shared portable child-group implementation.
//!
//! Production signals delegate to the original Ops registration. Injected
//! receivers exist only in this crate's tests and never become a public override.

use crate::tooling::{Failure, Result};
use rustix::process::Pid;
pub(crate) use simplestchat_ops::owned_process::reserved;
use std::sync::Arc;

fn failure(error: simplestchat_ops::owned_process::Error) -> Failure {
    use simplestchat_ops::owned_process::Error;
    Failure::new(
        1,
        match error {
            Error::Supervision => "Owned child supervision failed.",
            Error::Wait => "Owned child wait failed.",
            Error::Reap => "Owned child reap failed.",
        },
    )
}
pub(crate) async fn exited(pid: Pid) -> Result<()> {
    simplestchat_ops::owned_process::exited(pid)
        .await
        .map_err(failure)
}
pub(crate) fn exited_now(pid: Pid) -> Result<bool> {
    simplestchat_ops::owned_process::exited_now(pid).map_err(failure)
}
/// Preserve the existing CI API while the actual reservation lives in Ops.
pub(crate) struct Group(simplestchat_ops::owned_process::Group);
impl Group {
    pub(crate) fn new(child: std::process::Child) -> Self {
        Self(simplestchat_ops::owned_process::Group::new(child))
    }
    pub(crate) fn pid(&self) -> Pid {
        self.0.pid()
    }
    pub(crate) fn child(&mut self) -> &mut std::process::Child {
        self.0.child()
    }
    pub(crate) async fn reap(&mut self) -> Result<std::process::ExitStatus> {
        self.0.reap().await.map_err(failure)
    }
}
#[path = "owned_process_cleanup.rs"]
mod cleanup;
pub(crate) use cleanup::finish;

pub(crate) struct Signals {
    native: Option<simplestchat_ops::owned_process::Signals>,
    completion: Option<Arc<simplestchat_ops::owned_process::CompletionSignals>>,
    #[cfg(test)]
    injected: Option<tokio::sync::mpsc::UnboundedReceiver<u8>>,
}
impl Signals {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            native: Some(simplestchat_ops::owned_process::Signals::new().map_err(failure)?),
            completion: None,
            #[cfg(test)]
            injected: None,
        })
    }
    #[cfg(test)]
    pub(crate) fn injected(receiver: tokio::sync::mpsc::UnboundedReceiver<u8>) -> Self {
        Self {
            native: None,
            completion: None,
            injected: Some(receiver),
        }
    }
    /// Observe an existing synchronous registration without creating another.
    /// The whole caller retains the original Arc through runtime teardown and
    /// checks that all observational borrowers retired before checked finish.
    pub(crate) fn from_completion(
        owner: Arc<simplestchat_ops::owned_process::CompletionSignals>,
    ) -> Self {
        Self {
            native: None,
            completion: Some(owner),
            #[cfg(test)]
            injected: None,
        }
    }
    pub(crate) async fn next(&mut self) -> u8 {
        if let Some(owner) = &self.completion {
            loop {
                if let Some(code) = owner.status() {
                    return code;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        #[cfg(test)]
        if let Some(receiver) = &mut self.injected {
            return match receiver.recv().await {
                Some(code) => code,
                None => std::future::pending().await,
            };
        }
        self.native
            .as_mut()
            .expect("registered native signals")
            .next()
            .await
    }
    pub(crate) async fn pending(&mut self) -> Option<u8> {
        if let Some(owner) = &self.completion {
            return owner.status();
        }
        tokio::select! {
            biased;
            code = self.next() => Some(code),
            _ = tokio::task::yield_now() => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shared_typed_errors_keep_existing_ci_failure_text_and_status() {
        use simplestchat_ops::owned_process::Error;
        for (error, expected) in [
            (Error::Supervision, "Owned child supervision failed."),
            (Error::Wait, "Owned child wait failed."),
            (Error::Reap, "Owned child reap failed."),
        ] {
            let failure = super::failure(error);
            assert_eq!(failure.code, 1);
            assert_eq!(failure.message, expected);
            assert!(!failure.is_child_exit());
        }
    }
}
