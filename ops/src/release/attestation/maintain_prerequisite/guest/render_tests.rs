use super::*;

fn sources() -> BTreeMap<String, Vec<u8>> {
    let mut values = BTreeMap::new();
    macro_rules! input {
        ($name:literal) => {
            values.insert(
                $name.to_owned(),
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../", $name)).to_vec(),
            );
        };
    }
    input!("ops/ansible/templates/public-app.env.j2");
    input!("ops/ansible/templates/public-migration.env.j2");
    input!("ops/ansible/templates/public-proxy.env.j2");
    input!("ops/ansible/templates/public-compose.yml.j2");
    input!("ops/ansible/templates/public-pg_hba.conf.j2");
    input!("ops/ansible/templates/public-init-database.sql.j2");
    input!("ops/ansible/templates/public-runtime-grants.sql.j2");
    input!("ops/ansible/templates/public-postgres-admin-password.j2");
    input!("docker-compose.yml");
    input!("Caddyfile");
    values
}
fn secrets() -> BTreeMap<String, String> {
    SECRET_NAMES
        .into_iter()
        .enumerate()
        .map(|(i, name)| (name.into(), format!("{i:064x}")))
        .collect()
}
fn identity() -> Identity<'static> {
    Identity {
        image: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        revision: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        run_id: "11111111111111111111111111111111",
        postgres: "docker.io/library/postgres:18@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        caddy: CADDY,
        bridge: "scdddddddddddd",
    }
}
#[test]
fn fixed_render_keeps_quotes_dollar_interpolation_security_and_separate_secrets() {
    let inputs = sources();
    let values = secrets();
    let outputs = render(&inputs, identity(), &values).unwrap();
    assert_eq!(outputs.len(), 13);
    let app = std::str::from_utf8(&outputs["app.env"]).unwrap();
    for line in [
        "REGISTRATION_ENABLED=false\n",
        "MEDIA_WORKERS=1\n",
        "RTC_PORT_END=40000\n",
        "MAX_CONNECTIONS=175\n",
        "MAX_USERS=100\n",
        "ANNOUNCE_IPV6=\n",
    ] {
        assert!(app.contains(line));
    }
    assert!(!app.contains("TURN_"));
    assert!(app.contains(&format!("JWT_SECRET={}\n", values["jwt_secret"])));
    assert!(
        std::str::from_utf8(&outputs["init-database.sql"])
            .unwrap()
            .contains(&format!("PASSWORD '{}'", values["migration_password"]))
    );
    let public = std::str::from_utf8(&outputs["compose.public.yml"]).unwrap();
    assert_eq!(
        public
            .matches("simplestchat.maintain-prerequisite: \"11111111111111111111111111111111\"")
            .count(),
        4
    );
    for line in [
        "image: \"sha256:aaaaaaaa",
        "127.0.0.1:443:443/udp",
        "\"$$(cat /proc/1/comm)\"",
        "network_mode: none",
        "read_only: true",
        "no-new-privileges:true",
        "logging: *bounded-logging",
        "name: \"scdddddddddddd\"",
        "external: true",
    ] {
        assert!(public.contains(line), "{line}");
    }
    assert!(!public.contains("enable_ipv6"));
    let base = std::str::from_utf8(&outputs["compose.base.yml"]).unwrap();
    assert!(base.contains("${SIMPLESTCHAT_IMAGE:-simplestchat:local}"));
    assert!(
        base.contains("${TRUSTED_PROXY_SECRET:?set TRUSTED_PROXY_SECRET to at least 32 bytes}")
    );
    assert!(base.contains("127.0.0.1:40000:40000/udp"));
    assert!(!base.contains("${RTC_PORT_END"));
    assert_eq!(
        outputs["pg_hba.conf"],
        inputs["ops/ansible/templates/public-pg_hba.conf.j2"]
    );
    assert_eq!(
        outputs["runtime-grants.sql"],
        inputs["ops/ansible/templates/public-runtime-grants.sql.j2"]
    );
    assert_eq!(outputs["Caddyfile"], inputs["Caddyfile"]);
    let secrets_json: BTreeMap<String, String> =
        serde_json::from_slice(&outputs["secrets.json"]).unwrap();
    assert_eq!(secrets_json, values);
    assert_eq!(
        outputs["postgres-admin-password"],
        format!("{}\n", values["postgres_admin_password"]).into_bytes()
    );
}
#[test]
fn source_drift_and_all_external_identity_injections_fail_before_output() {
    for name in sources().keys() {
        let mut changed = sources();
        changed.get_mut(name).unwrap().push(b' ');
        assert!(render(&changed, identity(), &secrets()).is_err(), "{name}");
    }
    let mut invalid = identity();
    invalid.run_id = "123";
    assert!(render(&sources(), invalid, &secrets()).is_err());
    let mut invalid = identity();
    invalid.image = "sha256:aaa\"\n";
    assert!(render(&sources(), invalid, &secrets()).is_err());
    let mut invalid = identity();
    invalid.bridge = "scdddddddddddd\n";
    assert!(render(&sources(), invalid, &secrets()).is_err());
    let mut values = secrets();
    values.insert("owner_password".into(), "not hex".into());
    assert!(render(&sources(), identity(), &values).is_err());
    let mut values = secrets();
    values.insert("unexpected".into(), "0".repeat(64));
    assert!(render(&sources(), identity(), &values).is_err());
}
