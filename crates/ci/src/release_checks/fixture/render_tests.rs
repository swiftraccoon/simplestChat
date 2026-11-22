//! The fixture configuration rendered from the current deployment templates
//! by Ansible itself, with the play the fixture generates, and the fixture's
//! pins on the files it shares with the deployment, the workflow and the
//! browser helper. Ansible runs locally in a private directory; nothing is
//! started and no network is used.

use super::super::{plan, policy, render};
use super::{Scratch, TOKEN, checkout, oracle};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FILES: [&str; 12] = [
    "Caddyfile",
    "app.env",
    "browser-assets-installed.json",
    "compose.base.yml",
    "compose.public.yml",
    "images.json",
    "init-database.sql",
    "migration.env",
    "pg_hba.conf",
    "postgres-admin-password",
    "proxy.env",
    "runtime-grants.sql",
];

/// The fixture configuration of the current templates, rendered through
/// the fixture's own play, for config `/etc/simplestchat-public`.
fn rendered() -> &'static BTreeMap<String, String> {
    static RENDERED: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    RENDERED.get_or_init(|| {
        let scratch = Scratch::new("render");
        let staging = scratch.0.join("rendered");
        fs::create_dir(&staging).unwrap();
        let identity = render::Identity {
            image: IMAGE.into(),
            revision: "b".repeat(40),
            token: TOKEN.into(),
            postgres: plan::POSTGRES.into(),
            caddy: plan::CADDY.into(),
        };
        let (config, root) = (
            Path::new("/etc/simplestchat-public"),
            Path::new("/srv/simplestchat-public"),
        );
        let variables = render::variables(&identity, config, root, &oracle::secrets());
        let play = scratch.0.join("play.json");
        fs::write(
            &play,
            serde_json::to_vec(&render::play(&checkout(), &staging, &variables)).unwrap(),
        )
        .unwrap();
        let playbooks = checkout().join("ops/ansible");
        let output = std::process::Command::new(crate::automation::pins::ansible::ansible_tool(
            "ansible-playbook",
        ))
        .args(["-i".as_ref(), "localhost,".as_ref(), play.as_os_str()])
        .current_dir(&playbooks)
        .env("ANSIBLE_CONFIG", playbooks.join("ansible.cfg"))
        .env("ANSIBLE_HOME", scratch.0.join("home"))
        .env("ANSIBLE_LOCAL_TEMP", scratch.0.join("local"))
        .env("ANSIBLE_NOCOLOR", "1")
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let rendered = render::TEMPLATES
            .iter()
            .map(|(name, template)| {
                (
                    (*name).to_owned(),
                    fs::read_to_string(staging.join(template)).unwrap(),
                )
            })
            .collect();
        let sources = render::Sources {
            rendered,
            base: fs::read_to_string(checkout().join("docker-compose.yml")).unwrap(),
            caddyfile: fs::read_to_string(checkout().join("Caddyfile")).unwrap(),
        };
        render::outputs(&identity, config, root, &sources).unwrap()
    })
}

fn yaml(name: &str) -> Value {
    serde_saphyr::from_str(&rendered()[name]).unwrap()
}

fn environment(name: &str) -> BTreeMap<String, String> {
    rendered()[name]
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (key, value) = line.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

#[test]
fn every_fixture_file_is_rendered_and_both_app_selections_stay_quoted_immutable_ids() {
    assert!(rendered().keys().map(String::as_str).collect::<Vec<_>>() == FILES);
    for (name, text) in rendered() {
        assert!(
            text.ends_with('\n') && !text.contains("{{") && !text.contains("{%"),
            "{name}"
        );
    }
    let images: Value = serde_json::from_str(&rendered()["images.json"]).unwrap();
    assert!(
        images
            == json!({"revision": "b".repeat(40), "serverImage": IMAGE, "postgresImage": plan::POSTGRES,
            "caddyImage": plan::CADDY})
    );
    assert!(
        rendered()["compose.public.yml"]
            .matches(&format!("image: \"{IMAGE}\""))
            .count()
            == 2
    );
    let receipt: Value =
        serde_json::from_str(&rendered()["browser-assets-installed.json"]).unwrap();
    let digest = super::super::files::sha256(rendered()["Caddyfile"].as_bytes());
    assert!(receipt == json!({"schemaVersion": 1, "caddyfileSha256": digest}));
}

#[test]
fn only_loopback_ports_are_published_and_every_resource_carries_the_owner_label() {
    let (base, public) = (yaml("compose.base.yml"), yaml("compose.public.yml"));
    let app = &base["services"]["simplestchat"];
    assert!(app["ports"] == json!(["127.0.0.1:3000:3000", "127.0.0.1:40000:40000/udp"]));
    assert!(public["services"]["caddy"]["ports"] == json!(["127.0.0.1:443:443/tcp"]));
    assert!(public["services"]["simplestchat"].get("ports").is_none());
    assert!(
        public["services"]["simplestchat"]["extends"]
            == json!({"file": "./compose.base.yml", "service": "simplestchat"})
    );
    for services in [&base["services"], &public["services"]] {
        for (name, service) in services.as_object().unwrap() {
            assert!(service["labels"][plan::LABEL] == json!(TOKEN), "{name}");
        }
    }
    let names: Vec<&String> = public["services"].as_object().unwrap().keys().collect();
    assert!(names.len() == 4);
    assert!(public["networks"]["default"]["labels"][plan::LABEL] == json!(TOKEN));
    for name in ["postgres", "migrate"] {
        assert!(public["services"][name]["network_mode"] == json!("none"));
        assert!(public["services"][name].get("ports").is_none());
    }
    let volumes = public["services"]["caddy"]["volumes"].as_array().unwrap();
    for name in ["fixture.crt", "fixture.key"] {
        let mount = format!("/etc/simplestchat-public/{name}:/etc/caddy/{name}:ro");
        assert!(volumes.contains(&json!(mount)), "{name}");
    }
}

#[test]
fn runtime_privilege_and_database_role_restrictions_are_the_deployments() {
    let (base, public) = (yaml("compose.base.yml"), yaml("compose.public.yml"));
    assert!(public["services"]["postgres"]["user"] == json!("999:999"));
    for name in ["simplestchat", "migrate", "caddy"] {
        assert!(
            public["services"][name]["user"] == json!("10001:10001"),
            "{name}"
        );
    }
    let services = [
        &base["services"]["simplestchat"],
        &public["services"]["postgres"],
        &public["services"]["migrate"],
        &public["services"]["caddy"],
    ];
    for service in services {
        assert!(service["read_only"] == json!(true));
        assert!(service["cap_drop"] == json!(["ALL"]));
        assert!(service["security_opt"] == json!(["no-new-privileges:true"]));
    }
    assert!(public["services"]["migrate"]["profiles"] == json!(["maintenance"]));
    assert!(public["services"]["migrate"]["restart"] == json!("no"));
    let templates = checkout().join("ops/ansible/templates");
    for name in ["pg_hba.conf", "runtime-grants.sql"] {
        let template = fs::read_to_string(templates.join(format!("public-{name}.j2"))).unwrap();
        assert!(rendered()[name] == template, "{name}");
    }
    let bootstrap = &rendered()["init-database.sql"];
    assert!(
        bootstrap
            .matches("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")
            .count()
            == 2
    );
}

#[test]
fn the_origin_is_local_media_is_minimal_and_each_secret_is_its_own() {
    let (app, migration, proxy) = (
        environment("app.env"),
        environment("migration.env"),
        environment("proxy.env"),
    );
    for name in ["ALLOWED_ORIGINS", "WEBAUTHN_ORIGIN"] {
        assert!(app[name] == "https://localhost");
    }
    for (name, value) in [
        ("WEBAUTHN_RP_ID", "localhost"),
        ("ANNOUNCE_IP", "127.0.0.1"),
        ("RUN_MIGRATIONS", "false"),
        ("REGISTRATION_ENABLED", "false"),
        ("MEDIA_WORKERS", "1"),
        ("RTC_PORT_END", "40000"),
    ] {
        assert!(app[name] == value, "{name}");
    }
    assert!(migration["RUN_MIGRATIONS"] == "true");
    assert!(proxy["CADDY_DOMAIN"] == "localhost");
    assert!(proxy["CADDY_UPSTREAM"] == "simplestchat:3000");
    assert!(proxy["TRUSTED_PROXY_SECRET"] == app["TRUSTED_PROXY_SECRET"]);
    assert!(app["DATABASE_URL"].starts_with("postgres://simplestchat_app:"));
    assert!(migration["DATABASE_URL"].starts_with("postgres://simplestchat_migrate:"));
    assert_ne!(app["DATABASE_URL"], migration["DATABASE_URL"]);
    let administrator = rendered()["postgres-admin-password"].trim().to_owned();
    let selected = [
        app["METRICS_TOKEN"].clone(),
        app["TRUSTED_PROXY_SECRET"].clone(),
        administrator,
    ];
    let distinct: std::collections::BTreeSet<&String> = selected.iter().collect();
    assert!(distinct.len() == 3);
}

#[test]
fn tls_uses_the_fixture_certificate_without_acme_redirects_or_an_admin_listener() {
    let caddy = &rendered()["Caddyfile"];
    for present in [
        "\tadmin off\n",
        "\tauto_https off\n",
        "https://localhost {",
        "tls /etc/caddy/fixture.crt /etc/caddy/fixture.key",
        "reverse_proxy simplestchat:3000",
        "header_up X-SimplestChat-Proxy {$TRUSTED_PROXY_SECRET}",
        "@private path /metrics /metrics/* /diagnostics /diagnostics/*",
        "respond @private \"Not Found\" 404",
    ] {
        assert!(caddy.contains(present), "{present}");
    }
    assert!(
        !caddy.contains("tls_insecure_skip_verify") && !caddy.contains("simplestchat.example.com")
    );
}

#[test]
fn a_changed_anchor_refuses_the_rendering_instead_of_guessing() {
    let sources = || {
        let rendered = oracle::rendered_default();
        let mut templates = rendered.clone();
        let base = templates.remove("docker-compose.yml").unwrap();
        let caddyfile = templates.remove("Caddyfile").unwrap();
        render::Sources {
            rendered: templates,
            base,
            caddyfile,
        }
    };
    let identity = render::Identity {
        image: IMAGE.into(),
        revision: "b".repeat(40),
        token: TOKEN.into(),
        postgres: "docker.io/library/postgres:18.6-bookworm@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".into(),
        caddy: "docker.io/library/caddy:2.11.4-alpine@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(),
    };
    let (config, root) = (
        Path::new("/etc/simplestchat-public"),
        Path::new("/srv/simplestchat-public"),
    );
    render::outputs(&identity, config, root, &sources()).unwrap();
    let changed = "Production fixture anchor changed; review its rendering";
    let edits: [(&str, &str, &str, &str); 9] = [
        (
            "compose.public.yml",
            "      - \"443:443/udp\"\n",
            "",
            changed,
        ),
        (
            "compose.public.yml",
            "  caddy:\n",
            "  caddy:\n    labels:\n      x: y\n",
            changed,
        ),
        (
            "compose.public.yml",
            "/browser-assets:/run/simplestchat-browser-assets:ro\"\n",
            "/browser-assets:/run/simplestchat-browser-assets:ro\"\n      - \"/x:/y\"\n",
            changed,
        ),
        (
            "compose.public.yml",
            "services:\n",
            "networks: {}\nservices:\n",
            changed,
        ),
        (
            "compose.public.yml",
            &format!("image: \"{IMAGE}\"\n    user: \"10001:10001\"\n    network_mode"),
            "image: \"other\"\n    user: \"10001:10001\"\n    network_mode",
            changed,
        ),
        (
            "docker-compose.yml",
            "      - \"127.0.0.1:3000:3000\"\n",
            "      - \"127.0.0.1:3000:3000\"\n      - \"9:9\"\n",
            changed,
        ),
        (
            "Caddyfile",
            "reverse_proxy {$CADDY_UPSTREAM:127.0.0.1:3000}",
            "reverse_proxy other",
            changed,
        ),
        (
            "app.env",
            "MEDIA_WORKERS=1\n",
            "MEDIA_WORKERS=1\nMEDIA_WORKERS=1\n",
            "Production environment setting changed; review its rendering",
        ),
        (
            "app.env",
            "RTC_PORT_END=40000\n",
            "",
            "Production environment setting changed; review its rendering",
        ),
    ];
    for (name, from, to, message) in edits {
        let mut changed_sources = sources();
        let text = match name {
            "docker-compose.yml" => &mut changed_sources.base,
            "Caddyfile" => &mut changed_sources.caddyfile,
            _ => changed_sources.rendered.get_mut(name).unwrap(),
        };
        assert_eq!(text.matches(from).count(), 1, "{name} {from}");
        *text = text.replacen(from, to, 1);
        assert!(
            render::outputs(&identity, config, root, &changed_sources) == Err(message),
            "{name}"
        );
    }
}

#[test]
fn the_fixture_selects_the_deployments_own_database_and_proxy_images() {
    let group =
        fs::read_to_string(checkout().join("ops/ansible/group_vars/benchmark_hosts.yml")).unwrap();
    let group: Value = serde_saphyr::from_str(&group).unwrap();
    assert!(group["scpub_postgres_image"] == json!(plan::POSTGRES));
    assert!(group["scpub_caddy_image"] == json!(plan::CADDY));
    let workflow: Value = serde_saphyr::from_str(
        &fs::read_to_string(checkout().join(".github/workflows/ci.yml")).unwrap(),
    )
    .unwrap();
    assert!(workflow["jobs"]["deployment"]["env"]["CADDY_IMAGE"] == json!(plan::CADDY));
}

#[test]
fn the_failure_stage_inventory_is_the_browser_helpers_own() {
    let source = fs::read_to_string(checkout().join("web/e2e/release-assets.cjs")).unwrap();
    let mut found = std::collections::BTreeSet::from(["setup-or-cleanup".to_owned()]);
    for marker in ["atStage('", "new RolloverError('"] {
        for piece in source.split(marker).skip(1) {
            let stage: String = piece
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            if piece[stage.len()..].starts_with('\'') {
                found.insert(stage);
            }
        }
    }
    let known: std::collections::BTreeSet<String> = policy::ROLLOVER_STAGES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(found, known);
}

#[test]
fn the_fixture_and_the_browser_helper_run_the_ops_executable_the_workflow_installs() {
    let workflow: Value = serde_saphyr::from_str(
        &fs::read_to_string(checkout().join(".github/workflows/ci.yml")).unwrap(),
    )
    .unwrap();
    let steps = workflow["jobs"]["deployment"]["steps"].as_array().unwrap();
    let installed = steps
        .iter()
        .find(|step| {
            step["name"] == "Install the source-built Ops executable where a prepared host keeps it"
        })
        .unwrap();
    assert!(
        installed["run"]
            .as_str()
            .unwrap()
            .trim_end()
            .ends_with(plan::OPS)
    );
    let helper = fs::read_to_string(checkout().join("web/e2e/release-assets.cjs")).unwrap();
    assert!(helper.contains(&format!("const OPS = '{}';\n", plan::OPS)));
    assert!(helper.contains(
        "const DEPLOY_BUDGET = ['--timeout-seconds', '900', '--finalization-seconds', '60'];\n"
    ));
    let deploy = plan::release(plan::Action::Deploy, &"f".repeat(40));
    let words: Vec<String> = deploy
        .argv
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    assert!(words[5..] == ["--timeout-seconds", "900", "--finalization-seconds", "60"]);
}

#[test]
fn a_template_value_the_play_does_not_supply_fails_the_render_before_any_output() {
    let scratch = Scratch::new("undefined");
    let staging = scratch.0.join("rendered");
    fs::create_dir(&staging).unwrap();
    let identity = render::Identity {
        image: IMAGE.into(),
        revision: "b".repeat(40),
        token: TOKEN.into(),
        postgres: plan::POSTGRES.into(),
        caddy: plan::CADDY.into(),
    };
    let root = Path::new("/srv/simplestchat-public");
    let mut variables = render::variables(
        &identity,
        Path::new("/etc/simplestchat-public"),
        root,
        &oracle::secrets(),
    );
    variables.as_object_mut().unwrap().remove("scpub_max_rooms");
    let play = scratch.0.join("play.json");
    fs::write(
        &play,
        serde_json::to_vec(&render::play(&checkout(), &staging, &variables)).unwrap(),
    )
    .unwrap();
    let playbooks = checkout().join("ops/ansible");
    let output = std::process::Command::new(crate::automation::pins::ansible::ansible_tool(
        "ansible-playbook",
    ))
    .args(["-i".as_ref(), "localhost,".as_ref(), play.as_os_str()])
    .current_dir(&playbooks)
    .env("ANSIBLE_CONFIG", playbooks.join("ansible.cfg"))
    .env("ANSIBLE_HOME", scratch.0.join("home"))
    .env("ANSIBLE_LOCAL_TEMP", scratch.0.join("local"))
    .env("ANSIBLE_NOCOLOR", "1")
    .output()
    .unwrap();
    assert!(!output.status.success());
    assert!(!staging.join("public-app.env.j2").exists());
}
