//! Replays of `recorded_engines.json`: which command line the reference in
//! Python chose for an engine, from what this host has and what Podman
//! says of its connections.

use super::{Host, Name, prefix};
use crate::{
    Fault,
    document::Document,
    image::{
        policy::tests::{There, tallied},
        secrets::tests::{here, text, there},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

const RECORDING: &[u8] = include_bytes!("recorded_engines.json");

fn cases() -> Vec<Document> {
    let recording = Document::read(RECORDING).expect("a recording");
    let held = recording.get("engines").and_then(Document::items);
    held.expect("the engines").to_vec()
}

/// What Podman's connections were said to be, as bytes: a recorded text
/// is what was said, anything else the JSON that was said.
fn listed(case: &Document) -> Vec<u8> {
    match case.get("listed") {
        Some(Document::Text(said)) => said.clone().into_bytes(),
        Some(value) => value.spaced().into_bytes(),
        None => Vec::new(),
    }
}

/// Lists of connections and addresses that the reference failed on in its
/// interpreter's words: no list, no record, no JSON, no address or one that
/// is no text, and addresses whose brackets hold no IPv6 address.
const UNREAD: [(&str, &str); 24] = [
    (
        "podman whose connections are no list",
        "podman_connections_unread",
    ),
    (
        "podman whose connections hold no record",
        "podman_connections_unread",
    ),
    (
        "podman whose connections are no JSON",
        "podman_connections_unread",
    ),
    (
        "podman whose machine has no address",
        "podman_connections_unread",
    ),
    (
        "podman whose machine's address is no text",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at an IPv4 address in brackets",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at an unclosed bracket",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a bracket that is not opened",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is with text before its bracket",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is with text after its bracket",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a future address without its version",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is with a bracket in its user",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is with a bracket in its user before a name",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is of a scheme with a plus before brackets",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is of a scheme with a hyphen before brackets",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is of a scheme with a dot before brackets",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is of a scheme with a digit before brackets",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a scoped IPv6 address without its scope",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at an IPv6 address with two scopes",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a future address without what follows its dot",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a future address of no hexadecimal version",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a future address without its dot",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is at a bracket left open in its user",
        "podman_connections_unread",
    ),
    (
        "podman whose machine is with a bracket in its user before an IPv6 address without brackets",
        "podman_connections_unread",
    ),
];

#[test]
fn engines_match_the_recording() {
    let mut all = Vec::new();
    for case in cases() {
        let name = text(&case, "name").to_owned();
        let located: BTreeMap<String, PathBuf> = case
            .get("located")
            .and_then(Document::fields)
            .expect("the programs")
            .iter()
            .map(|(program, path)| (program.clone(), PathBuf::from(path.text().expect("a path"))))
            .collect();
        let present: BTreeSet<PathBuf> = case
            .get("present")
            .and_then(Document::items)
            .expect("what is present")
            .iter()
            .map(|path| PathBuf::from(path.text().expect("a path")))
            .collect();
        let failing = case.get("failing") == Some(&Document::Bool(true));
        let said = listed(&case);
        let mut asked = Vec::new();
        let locate = |program: &'static str| located.get(program).cloned();
        let there_is = |path: &Path| present.contains(path);
        let mut ask = |argv: &[OsString]| {
            let shown = argv.iter().map(|item| item.to_str().expect("text").into());
            asked.push(Document::List(shown.collect()));
            if failing {
                Err(Fault("command_failed"))
            } else {
                Ok(said.clone())
            }
        };
        let mut host = Host {
            located: &locate,
            home: Some(PathBuf::from(text(&case, "home"))),
            present: &there_is,
            mac: case.get("mac") == Some(&Document::Bool(true)),
            asked: &mut ask,
        };
        let engine = Name::parsed(text(&case, "engine")).expect("an engine");
        let came = prefix(engine, &mut host).map(|argv| {
            let shown = argv.iter().map(|item| item.to_str().expect("text").into());
            Document::List(shown.collect())
        });
        let recorded: Vec<Document> = case
            .get("asked")
            .and_then(Document::items)
            .expect("what was asked")
            .iter()
            .map(|call| {
                assert_eq!(
                    call.get("timeout").and_then(Document::whole),
                    Some(30),
                    "{name}"
                );
                call.get("argv").cloned().expect("a command line")
            })
            .collect();
        assert_eq!(asked, recorded, "{name}");
        all.push((name, here(came), there(&case)));
    }
    let tally = tallied(all, &[(&UNREAD, There::Raised)]);
    assert_eq!(tally, [20, 24, 24, 0, 0]);
}

#[test]
fn names_are_those_of_the_programs() {
    for (text, name, program) in [
        ("docker", Name::Docker, "docker"),
        ("podman", Name::Podman, "podman"),
    ] {
        assert_eq!(Name::parsed(text), Some(name));
        assert_eq!(name.program(), program);
    }
    for other in ["Docker", "nerdctl", "", "docker "] {
        assert_eq!(Name::parsed(other), None, "{other}");
    }
}

#[test]
fn a_home_that_is_unknown_leaves_the_host_socket_alone() {
    let locate = |_: &'static str| Some(PathBuf::from("/usr/bin/docker"));
    let there_is = |path: &Path| path == Path::new("/.docker/run/docker.sock");
    let mut ask = |_: &[OsString]| -> crate::Result<Vec<u8>> { panic!("nothing is asked") };
    let mut host = Host {
        located: &locate,
        home: None,
        present: &there_is,
        mac: false,
        asked: &mut ask,
    };
    assert_eq!(
        prefix(Name::Docker, &mut host),
        Err(Fault("local_docker_socket_missing"))
    );
}
