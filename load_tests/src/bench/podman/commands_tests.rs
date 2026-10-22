use super::*;
use std::path::Path;

fn options() -> Options {
    super::super::options::parse(
        &[
            "--server-image",
            "server:test",
            "--generator-image",
            "generator:test",
            "--output",
            "output",
            "--clients",
            "2",
            "--ramp-up",
            "1",
        ]
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>(),
        Path::new("/fixture"),
    )
    .unwrap()
}

#[test]
fn literal_commands_keep_media_in_the_owned_namespace_and_control_on_loopback() {
    let options = options();
    let scenario = &options.scenarios[0];
    let env = environment(&options, scenario, "private-token").unwrap();
    let args = server(&options, "bench-sfu-run", "run", &env);
    let index = args.iter().position(|arg| arg == "--publish").unwrap();
    assert_eq!(args[index + 1], "127.0.0.1:3129:3129/tcp");
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--network" || arg == "--privileged" || arg == "--volume")
    );
    let id = "a".repeat(64);
    let arguments =
        generator_args(&options, scenario, "run", "room", ("server", "generator")).unwrap();
    let args = generator(&options, "bench-gen-run", "run", &id, &arguments);
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--network", &format!("container:{id}")])
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--publish" || arg == "--privileged" || arg == "--volume")
    );
    assert!(args.windows(2).any(|pair| pair == ["--max-audio", "4"]));
    assert!(args.windows(2).any(|pair| pair == ["--max-video", "4"]));
    for args in [args, capture(&options, "bench-cap-run", "run", &id)] {
        assert!(args.windows(2).any(|pair| pair == ["--pull", "never"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--label", &format!("{LABEL}=run")])
        );
    }
    let args = netem(&options, "bench-netem-run", "run", &id, "fixed script");
    let entry = args.iter().position(|arg| arg == "--entrypoint").unwrap();
    assert_eq!(
        &args[entry + 1..],
        ["/bin/sh", &options.netem_image, "-c", "fixed script"]
    );
}

#[test]
fn experiment_settings_cannot_override_the_nine_runtime_wiring_keys() {
    for key in WIRED {
        let mut options = options();
        options.server_env.insert(key.into(), "unsafe".into());
        assert_eq!(
            environment(&options, &options.scenarios[0], "secret")
                .unwrap_err()
                .to_string(),
            format!("--server-env cannot override {key}")
        );
    }
    let mut options = options();
    options.server_env.insert("RUST_LOG".into(), "warn".into());
    assert_eq!(
        environment(&options, &options.scenarios[0], "secret").unwrap()["RUST_LOG"],
        "warn"
    );
}
