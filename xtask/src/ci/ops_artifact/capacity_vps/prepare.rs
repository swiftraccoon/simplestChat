//! One concrete preparation capability. The Native adapter borrows its existing
//! signal owner while it owns the lease and Ansible groups simultaneously.

use super::{Result, failure, tools::Invocation};
use crate::ci::command::Command;
use simplestchat_ops::capacity_vps::protocol::{Action, Header, Response, ResultData};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(in crate::ci) struct PrepareInvocation {
    lease: Option<Command>,
    ansible: Option<Command>,
    lease_proof: Arc<Invocation>,
    ansible_proof: Arc<Invocation>,
    header: Header,
    ready_reader: Mutex<Option<super::files::ReadyReader>>,
    accepted_ready: Mutex<Option<(u64, String)>>,
    completed: Arc<AtomicBool>,
}
impl std::fmt::Debug for PrepareInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateVpsPreparation")
    }
}
impl PrepareInvocation {
    pub(super) fn new(
        lease: Command,
        ansible: Command,
        header: Header,
        reader: super::files::ReadyReader,
        proofs: [Arc<Invocation>; 2],
    ) -> Self {
        let [lease_proof, ansible_proof] = proofs;
        Self {
            lease: Some(lease),
            ansible: Some(ansible),
            lease_proof,
            ansible_proof,
            header,
            ready_reader: Mutex::new(Some(reader)),
            accepted_ready: Mutex::new(None),
            completed: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(super) fn completion(&self) -> Completion {
        Completion(self.completed.clone())
    }
    pub(in crate::ci) fn take_commands(&mut self) -> Result<(Command, Command)> {
        if self.lease.is_none() || self.ansible.is_none() {
            return Err(failure(1));
        }
        Ok((self.lease.take().unwrap(), self.ansible.take().unwrap()))
    }
    pub(in crate::ci) fn check(&self) -> Result<()> {
        self.lease_proof.settled()?;
        self.ansible_proof.settled()
    }
    pub(in crate::ci) fn ready_bytes(&self, progress: (u64, &str)) -> Result<Option<Vec<u8>>> {
        self.check()?;
        self.ready_reader
            .lock()
            .map_err(|_| failure(1))?
            .as_ref()
            .ok_or_else(|| failure(1))?
            .read(progress)
    }
    pub(in crate::ci) fn ready(&self, bytes: &[u8]) -> Result<()> {
        self.check()?;
        let response =
            Response::decode(bytes, &self.header, Action::Reserve).map_err(|_| failure(1))?;
        if !matches!(response.result, ResultData::Reserve(_)) {
            return Err(failure(1));
        }
        self.check()?;
        let mut accepted = self.accepted_ready.lock().map_err(|_| failure(1))?;
        if accepted.is_some() {
            return Err(failure(1));
        }
        *accepted = Some((bytes.len() as u64, super::super::sha(bytes)));
        Ok(())
    }
    /// All commands and streams must have retired before this final admission.
    /// A ready record cannot substitute for either observed paired capture seal.
    pub(in crate::ci) fn finish(&self) -> Result<()> {
        let checked = self.check();
        let lease = self.lease_proof.captures();
        let ansible = self.ansible_proof.captures();
        let lease_exit = self.lease_proof.exit();
        let ansible_exit = self.ansible_proof.exit();
        let accepted = self
            .accepted_ready
            .lock()
            .map_err(|_| failure(1))
            .and_then(|ready| ready.clone().ok_or_else(|| failure(1)));
        let (mut reader, poisoned) = match self.ready_reader.lock() {
            Ok(reader) => (reader, false),
            Err(error) => (error.into_inner(), true),
        };
        let closed = reader
            .take()
            .ok_or_else(|| failure(1))
            .and_then(super::files::ReadyReader::finish);
        drop(reader);
        checked?;
        let lease = lease?;
        ansible?;
        closed?;
        // EOF may deliver bytes after the last live prefix observation. Only
        // the completed drainer seal can exclude that late suffix.
        let (count, sha256) = accepted?;
        if lease.stdout.bytes != count || lease.stdout.sha256 != sha256 {
            return Err(failure(1));
        }
        if [lease_exit?, ansible_exit?]
            .iter()
            .any(|exit| exit.status != Some(0) || !exit.settled)
        {
            return Err(failure(1));
        }
        self.check()?;
        if poisoned || self.completed.swap(true, Ordering::SeqCst) {
            return Err(failure(1));
        }
        Ok(())
    }
}
pub(super) struct Completion(Arc<AtomicBool>);
impl Completion {
    pub fn passed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
