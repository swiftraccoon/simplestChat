//! A move of one deployment to an empty host: both inventories and origins
//! explicit, the source live until the destination holds a verified copy, and
//! never restarted once destination startup was attempted.

mod activation;
mod checks;
mod options;
mod transaction;

pub(super) use options::{Options, USAGE, parse};
pub(super) use transaction::{native, phases};
