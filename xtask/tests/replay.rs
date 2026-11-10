//! The built xtask routes the reserved replay child before any other parsing.

use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

fn xtask(arguments: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
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
        let output = xtask(&["__replay"], Some(input));
        assert!(output.status.success(), "{:?}", output.status);
        assert_eq!(output.stdout, input);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn only_the_exact_single_argument_selects_the_replay_child() {
    // A command that receives the reserved word as an ordinary argument refuses
    // it with its usage, and the removed `ci`, `release`, `ops-artifact` and
    // `vps-capacity` routes are unknown commands.
    for (arguments, code) in [
        (&["__replay", "extra"][..], 1),
        (&["media-ip", "__replay"][..], 2),
        (&["ci", "release-controller", "__replay"][..], 1),
        (&["release", "verify", "__replay"][..], 1),
        (&["ops-artifact", "__replay"][..], 1),
        (&["vps-capacity", "__replay"][..], 1),
    ] {
        let output = xtask(arguments, None);
        assert_eq!(output.status.code(), Some(code), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
    }
}
