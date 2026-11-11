//! The built simplestchat-release routes the reserved replay child before any other parsing.

use std::{
    ffi::{OsStr, OsString},
    io::Write,
    os::unix::ffi::OsStringExt,
    process::{Command, Output, Stdio},
};

fn simplestchat_release<A: AsRef<OsStr>>(arguments: &[A], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_simplestchat-release"))
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Write from a thread while this one drains the child's output.
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().unwrap();
        let bytes = bytes.to_vec();
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
    let output = child.wait_with_output().unwrap();
    if let Some(writer) = writer {
        writer.join().unwrap().unwrap();
    }
    output
}

#[test]
fn reserved_replay_argument_copies_binary_input_byte_for_byte() {
    // Every byte value, NUL and invalid UTF-8 included, over several reads.
    let binary: Vec<u8> = (0..=255).cycle().take(5 * 8192 + 17).collect();
    for input in [&[][..], &binary[..]] {
        let output = simplestchat_release(&["__replay"], Some(input));
        assert!(output.status.success(), "{:?}", output.status);
        assert_eq!(output.stdout, input);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn only_the_exact_single_argument_selects_the_replay_child() {
    // Each command family receives the reserved word as an ordinary argument
    // and refuses it with its usage; with a second argument it is unknown.
    for (arguments, code) in [
        (&["__replay", "extra"][..], 1),
        (&["verify", "__replay"][..], 2),
        (&["deploy", "__replay"][..], 2),
        (&["ops-artifact", "__replay"][..], 2),
        (&["vps-capacity", "__replay"][..], 2),
    ] {
        let output = simplestchat_release(arguments, None);
        assert_eq!(output.status.code(), Some(code), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
    }
}

#[test]
fn verify_forwards_a_non_utf8_argument_before_the_utf8_router() {
    // The Ops verifier refuses the argument with its usage (status 2). Reaching
    // the UTF-8 router instead would panic in std::env::args (status 101).
    let argument = OsString::from_vec(vec![0xff]);
    let output = simplestchat_release(&[OsStr::new("verify"), argument.as_os_str()], None);
    assert_eq!(output.status.code(), Some(2), "{:?}", output.status);
    assert!(output.stdout.is_empty());
}

#[test]
fn fetch_forwards_to_the_ops_controller_before_the_utf8_router() {
    // The Ops fetch controller owns its options: its help names its own route
    // and a non-UTF-8 argument is its usage error, never the router's panic.
    let help = simplestchat_release(&["fetch", "--help"], None);
    assert_eq!(help.status.code(), Some(0), "{:?}", help.status);
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.starts_with("Usage: simplestchat-ops release fetch --repository REPO"));
    assert!(text.contains("installed Ops receiver"));
    let argument = OsString::from_vec(vec![0xff]);
    let output = simplestchat_release(&[OsStr::new("fetch"), argument.as_os_str()], None);
    assert_eq!(output.status.code(), Some(2), "{:?}", output.status);
    assert!(output.stdout.is_empty());
}
