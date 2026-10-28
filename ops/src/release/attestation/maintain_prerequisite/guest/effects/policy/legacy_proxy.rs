//! Fixed three-bind legacy proxy profile for the dedicated host upgrade case.
//! Exact original proxy profile with exactly three real mounts.
//! Call only for independently inspected host-proxy-upgrade bootstrap rows;
//! existing modern proxy_profile remains unchanged for every other caller.
use super::*;

pub(in super::super) fn legacy_proxy_profile(
    row: &Value,
    id: &str,
    image_id: &str,
    image_ref: &str,
    bridge: &str,
) -> Result<()> {
    let host = &row["HostConfig"];
    let config = &row["Config"];
    require(
        row["Id"] == id
            && row["Image"] == image_id
            && config["User"] == "10001:10001"
            && config["Image"] == image_ref
            && host["ReadonlyRootfs"] == true
            && host["CapDrop"] == json!(["ALL"])
            && host["CapAdd"] == json!(["CAP_NET_BIND_SERVICE"])
            && host["Init"] == true
            && host["Memory"].as_u64() == Some(256 * 1024 * 1024)
            && host["PidsLimit"].as_u64() == Some(128)
            && host["NanoCpus"].as_u64() == Some(500_000_000),
    )?;
    // Our raw-create and Compose profiles each select one enabled NNP option.
    // A neighboring override must not turn a matching true flag into authority.
    let security_options = host["SecurityOpt"].as_array().ok_or(Error::Refused)?;
    require(matches!(
        security_options.as_slice(),
        [Value::String(option)]
            if matches!(option.as_str(), "no-new-privileges" | "no-new-privileges:true")
    ))?;
    let expected = BTreeMap::from([
        (
            "/etc/caddy/Caddyfile",
            (format!("{CONFIG}/Caddyfile"), false),
        ),
        ("/data", (format!("{PUBLIC}/caddy-data"), true)),
        ("/config", (format!("{PUBLIC}/caddy-config"), true)),
    ]);
    let mounts = row["Mounts"].as_array().ok_or(Error::Refused)?;
    require(mounts.len() == expected.len())?;
    let mut seen = BTreeSet::new();
    for mount in mounts {
        let destination = string(&mount["Destination"])?;
        let (source, writable) = expected.get(destination).ok_or(Error::Refused)?;
        require(
            seen.insert(destination)
                && mount["Type"] == "bind"
                && mount["Source"] == source.as_str()
                && mount["RW"].as_bool() == Some(*writable),
        )?;
    }
    require(
        host["NetworkMode"] == bridge
            && host["RestartPolicy"] == json!({"Name":"unless-stopped","MaximumRetryCount":0})
            && host["PortBindings"]
                == json!({
                    "80/tcp":[{"HostIp":"127.0.0.1","HostPort":"80"}],
                    "443/tcp":[{"HostIp":"127.0.0.1","HostPort":"443"}],
                    "443/udp":[{"HostIp":"127.0.0.1","HostPort":"443"}]
                }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Value {
        // Finite inspection input; this is no daemon or TLS execution proof.
        json!({
            "Id":"a".repeat(64),"Image":format!("sha256:{}", "b".repeat(64)),
            "Config":{"User":"10001:10001","Image":"caddy@sha256:fixed"},
            "HostConfig":{
                "ReadonlyRootfs":true,"CapDrop":["ALL"],"CapAdd":["CAP_NET_BIND_SERVICE"],
                "Init":true,"Memory":268435456,"PidsLimit":128,"NanoCpus":500000000,
                "SecurityOpt":["no-new-privileges"],"NetworkMode":"sc0123456789ab",
                "RestartPolicy":{"Name":"unless-stopped","MaximumRetryCount":0},
                "PortBindings":{
                    "80/tcp":[{"HostIp":"127.0.0.1","HostPort":"80"}],
                    "443/tcp":[{"HostIp":"127.0.0.1","HostPort":"443"}],
                    "443/udp":[{"HostIp":"127.0.0.1","HostPort":"443"}]
                }
            },
            "Mounts":[
                {"Type":"bind","Destination":"/etc/caddy/Caddyfile",
                    "Source":"/etc/simplestchat-public/Caddyfile","RW":false},
                {"Type":"bind","Destination":"/data",
                    "Source":"/srv/simplestchat-public/caddy-data","RW":true},
                {"Type":"bind","Destination":"/config",
                    "Source":"/srv/simplestchat-public/caddy-config","RW":true}
            ]
        })
    }
    fn check(row: &Value) -> Result<()> {
        legacy_proxy_profile(
            row,
            &"a".repeat(64),
            &format!("sha256:{}", "b".repeat(64)),
            "caddy@sha256:fixed",
            "sc0123456789ab",
        )
    }

    #[test]
    fn legacy_and_modern_profiles_admit_only_their_actual_mount_sets() {
        let legacy = row();
        assert!(check(&legacy).is_ok());
        let mut modern = legacy.clone();
        modern["Mounts"].as_array_mut().unwrap().push(json!({
            "Type":"bind","Source":"/srv/simplestchat-public/browser-assets",
            "Destination":"/run/simplestchat-browser-assets","RW":false
        }));
        assert!(check(&modern).is_err());
        let modern_check = |row: &Value| {
            super::super::proxy_profile(
                row,
                &"a".repeat(64),
                &format!("sha256:{}", "b".repeat(64)),
                "caddy@sha256:fixed",
                "sc0123456789ab",
            )
        };
        assert!(modern_check(&modern).is_ok());
        assert!(modern_check(&legacy).is_err());
    }

    #[test]
    fn three_mounts_do_not_relax_identity_access_or_process_restrictions() {
        for (pointer, bad) in [
            ("/Image", json!(format!("sha256:{}", "c".repeat(64)))),
            ("/Config/User", json!("0:0")),
            ("/Mounts/0/RW", json!(true)),
            ("/Mounts/1/Source", json!("/srv/foreign")),
            ("/Mounts/2/Destination", json!("/data")),
            ("/HostConfig/ReadonlyRootfs", json!(false)),
            (
                "/HostConfig/SecurityOpt",
                json!(["no-new-privileges", "seccomp=unconfined"]),
            ),
            (
                "/HostConfig/CapAdd",
                json!(["CAP_NET_BIND_SERVICE", "CAP_SYS_ADMIN"]),
            ),
            (
                "/HostConfig/PortBindings/443~1tcp/0/HostIp",
                json!("0.0.0.0"),
            ),
        ] {
            let mut invalid = row();
            *invalid.pointer_mut(pointer).unwrap() = bad;
            assert!(check(&invalid).is_err(), "{pointer}");
        }
    }
}
