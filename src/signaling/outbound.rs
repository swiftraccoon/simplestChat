//! An essential queue overflow retires its socket through an independent signal.
//! Registrations are owned by connection handlers and keep only weak queue handles.
//! Ordinary fan-out does no registry lookup; the bounded connection set is searched
//! only after an essential enqueue fails. Repeated failures coalesce into one bit.

use crate::OutboundJson;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::{mpsc, watch};

struct Entry {
    sender: mpsc::WeakSender<OutboundJson>,
    resync: watch::Sender<bool>,
}

fn registrations() -> &'static Mutex<BTreeMap<u64, Entry>> {
    static ENTRIES: OnceLock<Mutex<BTreeMap<u64, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(Mutex::default)
}

/// Removes the registration even if connection dispatch is cancelled.
pub(crate) struct Registration {
    id: u64,
    signal: watch::Receiver<bool>,
}

impl Registration {
    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.signal.clone()
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        registrations()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

pub(crate) fn register(sender: &mpsc::Sender<OutboundJson>) -> Registration {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (resync, signal) = watch::channel(false);
    registrations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            id,
            Entry {
                sender: sender.downgrade(),
                resync,
            },
        );
    Registration { id, signal }
}

/// Wait without returning a watch read guard to the caller's `select!` output.
/// The guard is dropped here before any subsequent socket task await.
pub(crate) async fn wait_for_resync(signal: &mut watch::Receiver<bool>) {
    drop(signal.wait_for(|required| *required).await);
}

/// Terminal for this socket; a resumed session receives a fresh room snapshot.
/// Ephemeral speaker, layer and bandwidth hints must not call this function.
pub(crate) fn request_resync(sender: &mpsc::Sender<OutboundJson>) {
    let entries = registrations().lock().unwrap_or_else(|e| e.into_inner());
    for entry in entries.values() {
        if entry
            .sender
            .upgrade()
            .is_some_and(|candidate| candidate.same_channel(sender))
        {
            entry.resync.send_replace(true);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_targets_only_its_connection_and_survives_a_late_subscriber() {
        let (first, _first_rx) = mpsc::channel(1);
        let (second, _second_rx) = mpsc::channel(1);
        let first_registration = register(&first);
        let second_registration = register(&second);
        request_resync(&first.clone());
        request_resync(&first);
        assert!(*first_registration.subscribe().borrow());
        assert!(!*second_registration.subscribe().borrow());
        let id = first_registration.id;
        drop(first_registration);
        assert!(!registrations().lock().unwrap().contains_key(&id));
    }

    #[test]
    fn registration_does_not_keep_an_outbound_queue_alive() {
        let (sender, mut receiver) = mpsc::channel(1);
        let _registration = register(&sender);
        drop(sender);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }
}
