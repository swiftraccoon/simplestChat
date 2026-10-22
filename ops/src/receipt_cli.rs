//! One read-only local consistency check. Output contains the selected public
//! receipt projection and identities of both original snapshots, never unknown
//! source fields. Validation does not observe authentication or durable writes.

use simplestchat_ops::release::trust::{Envelope, ReceiverReceipt};
use std::path::Path;

pub(crate) fn inspect(envelope: &Path, receipt: &Path) -> Result<serde_json::Value, ()> {
    let envelope = Envelope::read(envelope).map_err(|_| ())?;
    let receipt = ReceiverReceipt::read(receipt, envelope.value()).map_err(|_| ())?;
    Ok(serde_json::json!({
        "receipt":receipt.value(),
        "envelopeSha256":envelope.sha256(),"envelopeBytes":envelope.as_json().len(),
        "receiptSha256":receipt.sha256(),"receiptBytes":receipt.as_json().len(),
    }))
}
