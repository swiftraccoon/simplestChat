#![forbid(unsafe_code)]

pub mod account;
pub mod common_passwords;
pub mod discovery;
pub mod invites;
pub mod jwt;
pub mod limiter;
pub mod notifications;
pub mod passkeys;
pub mod password;
pub mod routes;
pub mod session;
pub mod sessions;
pub mod types;
pub mod webauthn;

pub(crate) mod ws_tickets;
