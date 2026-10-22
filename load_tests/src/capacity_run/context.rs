//! Explicit resources and immutable settings shared by measured steps.

use super::{
    clock::Clock,
    engine::Engine,
    ownership::Ownership,
    process::Cancellation,
    types::{Browser, Shape},
};
use std::path::Path;

pub(super) struct Context<'a> {
    pub(super) engine: &'a dyn Engine,
    pub(super) clock: &'a dyn Clock,
    pub(super) cancellation: &'a Cancellation,
    pub(super) ownership: &'a Ownership,
    pub(super) run_id: &'a str,
    pub(super) server_image: &'a str,
    pub(super) generator_image: &'a str,
    pub(super) revisions: (String, String),
    pub(super) shape: Shape,
    pub(super) worker_threshold: f64,
    pub(super) server_memory: String,
    pub(super) generator_memory: String,
    pub(super) browser: &'a Browser,
    pub(super) output: &'a Path,
    pub(super) server_env: &'a [(String, String)],
}
