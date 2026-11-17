//! What a suite asks of its local engine: an image's identity and labels,
//! and the removal of exactly the sandbox a run owns.

use super::{
    hexadecimal,
    sandbox::{INPUT_LABEL, RUN_LABEL, canonical_image_id, immutable},
};
use crate::{Fault, Result, document::Document, require};
use std::{path::Path, time::Duration};

/// How long a question to the engine may take.
pub const ASKED: Duration = Duration::from_secs(30);
/// An engine's answer that is no JSON record of the expected kind.
const UNREAD: Fault = Fault("native_engine_unread");

/// The local engine, reached through its fixed prefix.
pub trait Engine {
    /// What the engine printed for `arguments` within `within`; refused
    /// unless it ended with 0, and refused once the run is cancelled.
    fn ask(&mut self, arguments: &[String], within: Duration) -> Result<Vec<u8>>;
    /// What the engine printed for `arguments` within `within`, as `ask`
    /// answers, but asked even once the run is cancelled: the removal of
    /// the run's own sandbox is what a cancelled run still owes.
    fn removing(&mut self, arguments: &[String], within: Duration) -> Result<Vec<u8>>;
    /// Run `arguments` with `input` on its standard input within `within`,
    /// its two streams appended to the private files `logs`, standard output
    /// in at most `room.0` bytes and standard error in `room.1`, and answer
    /// its exit status; refused once the run is cancelled.
    fn run(
        &mut self,
        arguments: &[String],
        input: &[u8],
        within: Duration,
        logs: (&Path, &Path),
        room: (u64, u64),
    ) -> Result<i32>;
}

/// Words as owned arguments.
pub fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The text field `name` of a record, if it is one.
fn text<'a>(value: &'a Document, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Document::text)
}

/// The labels of an inspected image or container.
fn labels(info: &Document) -> Result<&Document> {
    let labels = info.get("Config").and_then(|config| config.get("Labels"));
    labels
        .filter(|labels| labels.fields().is_some())
        .ok_or(UNREAD)
}

/// The inspection of the exact local image `image`, which must be of the
/// inputs `digest`, for Linux on `architecture`.
pub fn checked_image(
    engine: &mut dyn Engine,
    image: &str,
    digest: &str,
    architecture: &str,
) -> Result<Document> {
    require(immutable(image), "immutable_image_required")?;
    let asked = words(&["image", "inspect", "--format", "{{json .}}", image]);
    let info = Document::read(&engine.ask(&asked, ASKED)?).map_err(|_| UNREAD)?;
    require(info.fields().is_some(), UNREAD.0)?;
    let identity = text(&info, "Id").ok_or(UNREAD)?;
    require(
        canonical_image_id(identity)? == image,
        "image_identity_mismatch",
    )?;
    let bound = text(labels(&info)?, INPUT_LABEL) == Some(digest);
    require(bound, "image_inputs_mismatch")?;
    require(text(&info, "Os") == Some("linux"), "native_linux_required")?;
    let native = text(&info, "Architecture") == Some(architecture);
    require(native, "native_image_architecture_mismatch")?;
    Ok(info)
}

/// Stop and remove the run `run_id`'s sandbox of the image `image`, if
/// there is one: only by its full identity, and only while its labels and
/// image are still the run's. A cancelled run still asks all of this, as
/// the reference unwound its cancellation through its removal.
pub fn cleanup(engine: &mut dyn Engine, run_id: &str, image: &str) -> Result<()> {
    let filter = format!("label={RUN_LABEL}={run_id}");
    let listing = [
        "ps",
        "--all",
        "--no-trunc",
        "--filter",
        &filter,
        "--format",
        "{{.ID}}",
    ];
    let listed = engine.removing(&words(&listing), ASKED)?;
    let listed = String::from_utf8(listed).map_err(|_| UNREAD)?;
    let identifiers: Vec<&str> = listed.lines().collect();
    require(identifiers.len() <= 1, "unexpected_owned_containers")?;
    for identifier in identifiers {
        require(hexadecimal(identifier, 64), "container_identity")?;
        let asked = words(&["inspect", "--format", "{{json .}}", identifier]);
        let info = Document::read(&engine.removing(&asked, ASKED)?).map_err(|_| UNREAD)?;
        // Each condition is asked only while the earlier ones hold.
        let owned = text(&info, "Id") == Some(identifier)
            && canonical_image_id(text(&info, "Image").ok_or(UNREAD)?)? == image
            && text(labels(&info)?, RUN_LABEL) == Some(run_id);
        require(owned, "container_ownership_changed")?;
        engine.removing(&words(&["rm", "--force", identifier]), ASKED)?;
    }
    Ok(())
}
