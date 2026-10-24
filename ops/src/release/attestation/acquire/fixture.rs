//! Closed inert metadata/HTTP/cryptographic observations, never network proof.
use super::cached;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
pub(crate) struct Fixture {
    inner: cached::Fixture,
    pub root: PathBuf,
    pub candidate: PathBuf,
    pub bin: PathBuf,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub selection: Value,
}
pub(crate) fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(path)?;
    let written = file.write_all(bytes).and_then(|()| file.flush());
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    written.and(closed)
}
impl Fixture {
    pub fn new() -> Self {
        let inner = cached::Fixture::new();
        let selection = serde_json::to_value(&inner.selection).unwrap();
        let this = Self {
            root: inner.root.clone(),
            candidate: inner.candidate.clone(),
            bin: inner.bin.clone(),
            cwd: inner.cwd.clone(),
            home: inner.home.clone(),
            selection,
            inner,
        };
        this.install().unwrap();
        this
    }
    fn install(&self) -> io::Result<()> {
        let revision = &self.selection["revision"];
        let artifact = json!({"id":self.selection["artifactId"],"expired":false,
            "name":format!("simplestchat-production-{}",revision.as_str().unwrap()),
            "size_in_bytes":self.selection["artifactZipBytes"],
            "digest":format!("sha256:{}",self.selection["zipSha256"].as_str().unwrap()),
            "workflow_run":{"id":789,"head_sha":revision,"repository_id":17,
                "head_repository_id":17}});
        let run = json!({"id":789,"run_attempt":1,"head_branch":"main","name":"CI",
            "path":".github/workflows/ci.yml","event":"push","head_sha":revision,
            "status":"completed","conclusion":"success","repository":{"full_name":"owner/repo"},
            "head_repository":{"full_name":"owner/repo"}});
        write(
            &self.home.join("artifact.json"),
            &serde_json::to_vec(&artifact)?,
            0o600,
        )?;
        write(
            &self.home.join("run.json"),
            &serde_json::to_vec(&run)?,
            0o600,
        )?;
        write(
            &self.home.join("api.http"),
            concat!(
                "HTTP/1.1 302 Found\r\n",
                "Location: https://inert.blob.core.windows.net/p?q=INERT\r\n",
                "Content-Length: 0\r\n\r\n"
            )
            .as_bytes(),
            0o600,
        )?;
        let zip = cached::read(&self.candidate.join("artifact.zip"))?;
        let mut wire =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", zip.len()).into_bytes();
        wire.extend(zip);
        write(&self.home.join("blob.http"), &wire, 0o600)?;
        write(
            &self.bin.join("gh"),
            concat!(
                "#!/bin/sh\n[ \"${LC_ALL-}\" = C ] || exit 91\n",
                "if IFS= read -r input; then exit 92; fi\n",
                "printf '%s\\n' --call-- \"$@\" >> \"$HOME/argv\" || exit 93\n",
                "case \"$1:$2\" in\n",
                "api:*) case \"$8\" in *artifacts*) exec /bin/cat \"$HOME/artifact.json\";; ",
                "*runs*) exec /bin/cat \"$HOME/run.json\";; *) exit 94;; esac;;\n",
                "auth:token) printf INERT_TOKEN;;\n",
                "attestation:verify) exec /bin/cat \"$HOME/response.json\";;\n",
                "*) exit 95;; esac\n"
            )
            .as_bytes(),
            0o700,
        )?;
        write(
            &self.bin.join("curl"),
            concat!(
                "#!/bin/sh\nif [ \"${2-}\" = --version ]; then ",
                "printf 'curl 8.7.1 fixture\\nProtocols: https\\n'; exit 0; fi\n",
                "[ \"${GH_TOKEN+x}${GITHUB_TOKEN+x}\" = '' ] || exit 96\n",
                "config=$(/bin/cat) || exit 97\n",
                "case \"$config\" in *api.github.com*) ",
                "exec /bin/cat \"$HOME/api.http\";;\n",
                "*) exec /bin/cat \"$HOME/blob.http\";; esac\n"
            )
            .as_bytes(),
            0o700,
        )
    }
    pub fn environment(&self) -> BTreeMap<OsString, OsString> {
        BTreeMap::from([
            ("PATH".into(), self.bin.as_os_str().to_owned()),
            ("HOME".into(), self.home.as_os_str().to_owned()),
            ("GH_TOKEN".into(), "INERT_ONLY_GH_ROLE".into()),
        ])
    }
    pub fn arguments(&self, cached: bool) -> Vec<OsString> {
        let mut args = vec![
            "--repository".into(),
            "owner/repo".into(),
            "--revision".into(),
            self.selection["revision"].as_str().unwrap().into(),
            "--artifact-id".into(),
            "123".into(),
            "--ci-run".into(),
            "789".into(),
            "--output-parent".into(),
            self.root.join("output").into(),
            "--timeout-seconds".into(),
            "60".into(),
            "--finalization-seconds".into(),
            "10".into(),
        ];
        if cached {
            args.extend([
                "--artifact-dir".into(),
                self.candidate.as_os_str().to_owned(),
            ]);
        }
        args
    }
    pub fn snapshot(&self) -> io::Result<BTreeMap<String, (u64, String, u32)>> {
        self.inner.snapshot()
    }
    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        cached::read(&self.home.join(name))
    }
    pub fn attempts(&self) -> io::Result<Vec<PathBuf>> {
        let parent = self.root.join("output");
        if !parent.exists() {
            return Ok(Vec::new());
        }
        fs::read_dir(parent)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }
}
