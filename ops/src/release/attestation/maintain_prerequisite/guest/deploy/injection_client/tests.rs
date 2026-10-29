//! Inert framing/original/FD tests; no mount, Docker, privileged context or exec.
use super::context::{Config, Fence, Original};
use super::*;
use std::{
    fs,
    io::Write,
    os::{
        fd::IntoRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
        },
    },
    sync::atomic::{AtomicU64, Ordering},
};
fn config() -> Value {
    json!({"schemaVersion":1,"nonce":"a".repeat(32),"bodyEnd":10.0,
    "image":format!("sha256:{}","b".repeat(64)),"realCliSha256":"c".repeat(64),
    "realCliStamp":[1,2,3,4,5,6,7,8,9],"nativeSha256":"d".repeat(64),
    "nativeStamp":[9,8,7,6,5,4,3,2,1],"namespaceInode":1})
}
#[test]
fn only_exact_native_inspection_is_intercepted() {
    let args = [
        "--host",
        "unix:///var/run/docker.sock",
        "inspect",
        "--format",
        NATIVE_FORMAT,
        &"a".repeat(64),
    ]
    .map(OsString::from);
    assert_eq!(identifier(&args), Some("a".repeat(64).as_str()));
    for at in 0..args.len() {
        let mut changed = args.clone();
        changed[at] = "unknown".into();
        assert!(identifier(&changed).is_none());
    }
    let mut extra = args.to_vec();
    extra.push("extra".into());
    assert!(identifier(&extra).is_none());
}
#[test]
fn configuration_preserves_exact_nine_fields_and_integer_stamps() {
    assert!(Config::decode(&serde_json::to_vec(&config()).unwrap()).is_ok());
    for (key, value) in [
        ("schemaVersion", json!(true)),
        ("nonce", json!("A".repeat(32))),
        ("namespaceInode", json!(0)),
        ("bodyEnd", json!("10")),
        ("nativeStamp", json!([1, 2, 3, 4, 5, 6, 7, 8, true])),
        ("realCliStamp", json!([1, 2])),
        ("nativeSha256", json!("private")),
    ] {
        let mut changed = config();
        changed[key] = value;
        assert!(
            Config::decode(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{key}"
        );
    }
    let mut extra = config();
    extra["raw"] = "private".into();
    assert!(Config::decode(&serde_json::to_vec(&extra).unwrap()).is_err());
    assert!(Config::decode(b"{\"schemaVersion\":1,\"schemaVersion\":1}").is_err());
}
#[test]
fn bounded_frame_decoder_rejects_truncation_duplicates_bom_and_oversize() {
    let mut invalid = vec![
        0u32.to_be_bytes().to_vec(),
        ((FRAME + 1) as u32).to_be_bytes().to_vec(),
        vec![0, 0],
        vec![0, 0, 0, 5, b'{'],
    ];
    for body in [b"{\"x\":1,\"x\":2}".as_slice(), b"{} {}", b"\xef\xbb\xbf{}"] {
        let mut raw = (body.len() as u32).to_be_bytes().to_vec();
        raw.extend(body);
        invalid.push(raw);
    }
    for raw in invalid {
        let (mut input, mut output) = UnixStream::pair().unwrap();
        output.write_all(&raw).unwrap();
        nix::unistd::close(output).unwrap();
        let mut fence = Fence::fixture(2.0).unwrap();
        let observed = wire::receive(&mut input, &fence);
        let closed = nix::unistd::close(input);
        let finished = fence.finish();
        assert!(observed.is_err());
        assert!(closed.is_ok());
        assert!(finished.is_ok());
    }
}
#[test]
fn acknowledgement_requires_matching_completion_and_actual_eof() {
    for mode in 0..4 {
        let (mut input, mut output) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut fence = Fence::fixture(2.0).unwrap();
            let body = (|| {
                let ack = wire::receive(&mut output, &fence)?;
                require(
                    ack == json!({"action":"received","nonce":"a".repeat(32),
                    "receiptSha256":null}),
                )?;
                let mut complete = json!({"action":"complete","nonce":"a".repeat(32),
                    "receiptSha256":null});
                if mode == 1 {
                    complete["nonce"] = "b".repeat(32).into();
                }
                if mode == 2 {
                    complete["receiptSha256"] = "c".repeat(64).into();
                }
                wire::send(&mut output, &complete, &fence)?;
                if mode == 3 {
                    wire::write(&mut output, b"x", &fence)?;
                }
                Ok(())
            })();
            let closed = nix::unistd::close(output);
            let finished = fence.finish();
            (body, closed, finished)
        });
        let mut fence = Fence::fixture(2.0).unwrap();
        let observed = wire::completed(&mut input, &"a".repeat(32), None, &fence);
        let closed = nix::unistd::close(input);
        let finished = fence.finish();
        let server = worker.join().unwrap();
        assert_eq!(observed.is_ok(), mode == 0);
        assert!(closed.is_ok());
        assert!(finished.is_ok());
        assert!(server.0.is_ok());
        assert!(server.1.is_ok());
        assert!(server.2.is_ok());
    }
}
#[test]
fn receipt_requires_the_whole_actual_response_and_stopped_candidate_record() {
    let out = b"original response";
    let err = b"";
    let value = json!({"schemaVersion":1,"scope":"HostDeployInspectionBarrier",
        "nonce":"a".repeat(32),"identifier":"b".repeat(64),
        "image":format!("sha256:{}","c".repeat(64)),"responseRawExit":0,
        "stdoutBytes":out.len(),"stdoutSha256":hash(out),"stderrBytes":0,
        "stderrSha256":hash(err),"runningBefore":true,"runningAfter":false,
        "stopRawExit":0,"finishedChanged":true});
    let check = |value: &Value| {
        let raw = serde_json::to_vec(value).unwrap();
        wire::receipt_value(
            &raw,
            &hash(&raw),
            &"a".repeat(32),
            &format!("sha256:{}", "c".repeat(64)),
            &"b".repeat(64),
            out,
            err,
        )
    };
    assert!(check(&value).is_ok());
    for (key, new) in [
        ("stopRawExit", json!(1)),
        ("runningAfter", json!(true)),
        ("finishedChanged", json!(false)),
        ("stdoutBytes", json!(0)),
        ("stdoutSha256", json!("0".repeat(64))),
        ("responseRawExit", json!(true)),
    ] {
        let mut changed = value.clone();
        changed[key] = new;
        assert!(check(&changed).is_err());
    }
}
static NEXT: AtomicU64 = AtomicU64::new(0);
fn temporary() -> std::path::PathBuf {
    let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "injection-client-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn original_replacement_and_inherited_close_keep_the_actual_parent_file() {
    let root = temporary();
    let path = root.join("original");
    fs::write(&path, b"owned bytes").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut fence = Fence::fixture(2.0).unwrap();
    let mut original = Original::fixture(path.to_str().unwrap(), 32, 0o600, &fence).unwrap();
    let data = original.data(&fence).unwrap();
    fs::rename(&path, root.join("previous")).unwrap();
    fs::write(&path, b"owned bytes").unwrap();
    let refused = original.check();
    let closed = original.close();
    let finished = fence.finish();
    let parent = fs::File::open(root.join("previous")).unwrap();
    let duplicate = parent.try_clone().unwrap().into_raw_fd();
    let args = [
        "--enter".into(),
        duplicate.to_string().into(),
        "a".repeat(40).into(),
    ];
    let mut inherited = namespace::Inherited::parse(&args, true).unwrap();
    let expected = parent.metadata().unwrap();
    let inherited_close = inherited.close();
    // Parallel tests may immediately reuse this numeric FD for a different file.
    let still_original = fs::metadata(format!("/proc/self/fd/{duplicate}"))
        .is_ok_and(|now| now.dev() == expected.dev() && now.ino() == expected.ino());
    let parent_metadata = parent.metadata();
    let parent_close = nix::unistd::close(parent);
    fs::remove_file(path).unwrap();
    fs::remove_file(root.join("previous")).unwrap();
    fs::remove_dir(root).unwrap();
    assert_eq!(data, b"owned bytes");
    assert!(refused.is_err());
    assert!(closed.is_ok());
    assert!(finished.is_ok());
    assert!(inherited_close.is_ok());
    assert!(!still_original);
    assert!(parent_metadata.is_ok());
    assert!(parent_close.is_ok());
}
#[test]
fn private_mount_profile_rejects_shared_master_and_malformed_rows() {
    assert!(namespace::private_mountinfo(b"1 0 0:1 / / rw - ext4 root rw\n").is_ok());
    for raw in [
        b"".as_slice(),
        b"broken",
        b"1 0 0:1 / / rw shared:1 - ext4 root rw\n",
        b"1 0 0:1 / / rw master:1 - ext4 root rw\n",
        b"1 0 0:1 / / rw propagate_from:1 - ext4 root rw\n",
    ] {
        assert!(namespace::private_mountinfo(raw).is_err());
    }
}
