//! Private inert local tools plus a closed two-frame SSH worker, no host proof.
use super::fresh;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, fs, io, path::Path};
pub(crate) struct Fixture {
    pub inner: fresh::Fixture,
}
fn hash(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
impl Fixture {
    pub fn new() -> Self {
        let inner = fresh::Fixture::new();
        fresh::write(&inner.root.join("identity"), b"INERT IDENTITY\n", 0o600).unwrap();
        let receipt = json!({"schemaVersion":1,"status":"complete","passed":true,
            "settled":true,"phase":"complete","failureClass":null,
            "evidence":format!("/srv/simplestchat-public/releases/{}/download.inert",
                inner.selection["revision"].as_str().unwrap()),
            "revision":inner.selection["revision"],"artifactId":123,
            "zipSha256":inner.selection["zipSha256"],
            "artifactZipBytes":inner.selection["artifactZipBytes"],
            "downloadedBytes":inner.selection["artifactZipBytes"],
            "archiveSha256":hash(&inner.candidate.join("image.tar")),
            "manifestSha256":hash(&inner.candidate.join("release.json")),
            "elapsedSeconds":0.1,"finalized":true});
        fresh::write(
            &inner.home.join("receipt"),
            &serde_json::to_vec(&receipt).unwrap(),
            0o600,
        )
        .unwrap();
        let this = Self { inner };
        this.script(concat!(
            "IFS= read -r envelope || exit 92\n",
            "printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"ready\"}'\n",
            "IFS= read -r grant || exit 93\n",
            "printf '%s\\n' \"$grant\" > \"$HOME/grant-observed\" || exit 94\n",
            "/bin/cat \"$HOME/receipt\"\nprintf '\\n'\n"
        ))
        .unwrap();
        this
    }
    pub fn script(&self, body: &str) -> io::Result<()> {
        let mut script = concat!(
            "#!/bin/sh\n",
            "[ \"${GH_TOKEN+x}${GITHUB_TOKEN+x}${XDG_CONFIG_HOME+x}\" = '' ] || exit 90\n",
            "[ \"${LC_ALL-}\" = C ] || exit 91\n"
        )
        .to_owned();
        script.push_str(body);
        fresh::write(&self.inner.bin.join("ssh"), script.as_bytes(), 0o700)
    }
    pub fn arguments(&self, cached: bool) -> Vec<OsString> {
        let mut values = self.inner.arguments(false);
        values.extend([
            "--host".into(),
            "inert.example".into(),
            "--user".into(),
            "root".into(),
            "--identity".into(),
            self.inner.root.join("identity").into(),
        ]);
        if cached {
            values.extend([
                "--verified-directory".into(),
                self.inner.candidate.clone().into(),
            ]);
        }
        values
    }
    pub fn receipt(&self) -> Value {
        serde_json::from_slice(&fs::read(self.inner.home.join("receipt")).unwrap()).unwrap()
    }
}
