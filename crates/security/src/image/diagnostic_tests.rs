//! Replays of the scanner diagnostics of `recorded_sandbox.json`: what the
//! reference in Python made of each run, its container and its log.

use super::{Scanned, diagnostic};
use crate::{
    document::Document,
    image::{
        notices::tests::built,
        policy::tests::{There, tallied},
        sandbox::{
            Limits,
            tests::{cases, whole},
        },
        secrets::tests::{here, text, there},
    },
    scratch::Scratch,
};

/// Diagnostics that the reference failed on in its interpreter's words: a
/// log that is a link, and an outcome of its command that is not there,
/// no record or no JSON.
const UNREAD: [(&str, &str); 4] = [
    ("a log that is a link", "image_scanner_diagnostic_log"),
    ("a log without its outcome", "tool_entry_missing"),
    (
        "a log whose outcome is no record",
        "image_scanner_diagnostic_log",
    ),
    ("a log whose outcome is no JSON", "image_report_invalid"),
];

#[test]
fn diagnostics_match_the_recording() {
    let mut all = Vec::new();
    let mut wider = 0;
    for case in cases("diagnostics") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("diagnostic");
        let root = built(&case, &scratch.0);
        let logged = case.get("log") == Some(&Document::Bool(true));
        let log = logged.then(|| root.join("01-docker.log"));
        let mut limits = Limits::REVIEWED;
        if let Some(limit) = case.get("limit").and_then(Document::whole) {
            limits.log = u64::try_from(limit).expect("a bound");
        }
        let status = case.get("status").and_then(Document::items).map(|status| {
            let code = status.first().and_then(Document::whole).expect("a status");
            i32::try_from(code).expect("a status")
        });
        let owned = case.get("owned").filter(|owned| **owned != Document::Null);
        let scanned = Scanned {
            tool: text(&case, "tool"),
            status,
            owned,
            cleaned: case.get("cleanup") == Some(&Document::Bool(true)),
            scratch: whole(&case, "scratch"),
            log: log.as_deref(),
        };
        let came = diagnostic(&scanned, &limits);
        if name == "a scan whose container exit is past a word" {
            // The reference kept an exit status no machine word holds; a
            // document's whole numbers are those one holds.
            let result = case.get("result").and_then(|result| result.get("held"));
            let mut recorded = result.cloned().expect("a diagnostic");
            recorded.put("containerExitStatus", Document::Null);
            assert_eq!(came, Ok(recorded));
            wider += 1;
            continue;
        }
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(wider, 1);
    assert_eq!(tallied(all, &[(&UNREAD, There::Raised)]), [42, 3, 4, 0, 0]);
}
