//! Independent checksum actor policy; no claim about native validation isolation.
use super::*;
pub(in super::super) const SCRIPT: &str =
    "for file in /app/migrations/*.sql; do sha384sum \"$file\" || exit; done";
pub(in super::super) fn profile(
    row: &Value,
    id: &str,
    image: &str,
    name: &str,
    token: &str,
) -> Result<()> {
    let config = &row["Config"];
    let host = &row["HostConfig"];
    require(
        row["Id"] == id
            && row["Name"] == format!("/{name}")
            && row["Image"] == image
            && config["Labels"]["simplestchat.stage-proof"] == token
            && config["User"] == "10001:10001"
            && config["Entrypoint"] == json!(["/usr/bin/timeout"])
            && config["Cmd"]
                == json!([
                    "--signal=TERM",
                    "--kill-after=2s",
                    "10s",
                    "/bin/sh",
                    "-c",
                    SCRIPT
                ])
            && host["LogConfig"]
                == json!({"Type":"local","Config":{
            "max-size":"1m","max-file":"1","compress":"false"}})
            && host["Init"] == true
            && host["NetworkMode"] == "none"
            && host["ReadonlyRootfs"] == true
            && host["Privileged"] == false
            && host["CapDrop"] == json!(["ALL"])
            && (host["CapAdd"].is_null() || host["CapAdd"] == json!([]))
            && (host["SecurityOpt"] == json!(["no-new-privileges"])
                || host["SecurityOpt"] == json!(["no-new-privileges:true"]))
            && host["Memory"].as_u64() == Some(128 * 1024 * 1024)
            && host["NanoCpus"].as_u64() == Some(500_000_000)
            && host["PidsLimit"].as_u64() == Some(32)
            && (row["Mounts"].is_null() || row["Mounts"] == json!([])),
    )
}
pub(in super::super) fn migrations(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    require(bytes.len() <= 2 * 1024 * 1024 && bytes.is_ascii())?;
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Refused)?;
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let (checksum, path) = line
            .split_once("  /app/migrations/")
            .ok_or(Error::Refused)?;
        require(hex(checksum, 96))?;
        let (version, suffix) = path.split_once('_').ok_or(Error::Refused)?;
        require(!version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()))?;
        let suffix = suffix.strip_suffix(".sql").ok_or(Error::Refused)?;
        require(
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        )?;
        let version = version.trim_start_matches('0');
        let version = if version.is_empty() { "0" } else { version };
        require(result.len() < 10000 && result.insert(version.into(), checksum.into()).is_none())?;
    }
    require(!result.is_empty())?;
    Ok(result)
}
