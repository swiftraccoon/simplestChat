//! The command line: public selectors only, validated before any local file
//! is created or any host is contacted.

use super::{Result, require};
use regex::Regex;

pub(super) const USAGE: &str = "\
usage: simplestchat-release bootstrap --host HOST --name ALIAS --revision SHA \\
       --identity PATH --inventory PATH --output DIRECTORY [OPTION ...]

Enroll trusted SSH access to one private Debian benchmark host and,
with --provision, prepare it through ops/ansible/site.yml.

Required:
  --host HOST                       address or name of the host
  --name ALIAS                      inventory alias, also the key comment
  --revision SHA                    full commit; must equal the checkout's HEAD
  --identity PATH                   dedicated Ed25519 key outside the checkout;
                                    created when absent, never replaced
  --inventory PATH                  ops/ansible/inventory.local.* (ignored by Git)
  --output DIRECTORY                new evidence directory

Options:
  --user NAME                       login user                       debian
  --port N                          SSH port                         22
  --known-hosts PATH                host trust file                  ~/.ssh/known_hosts
  --host-fingerprint SHA256:...     independently verified Ed25519 fingerprint
  --password-file PATH              owner-only file outside the checkout;
                                    without it the password is prompted for
  --replacement-password-file PATH  saved before a forced password change
  --become-password-file PATH       protected sudo password file
  --minimum-free-gib N              required free space              20
  --provision                       run site.yml after enrollment
  --initial-maintenance             one-run OS updates and reboot; needs --provision
";

/// What the operator selected; no credential is among it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Options {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub name: String,
    pub revision: String,
    pub identity: String,
    pub known_hosts: String,
    pub host_fingerprint: Option<String>,
    pub inventory: String,
    pub output: String,
    pub password_file: Option<String>,
    pub replacement_password_file: Option<String>,
    pub become_password_file: Option<String>,
    pub minimum_free_gib: u64,
    pub provision: bool,
    pub initial_maintenance: bool,
}

/// A command line that asked for help, or was not understood.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Request {
    Help,
    /// The arguments do not form an invocation; the text says why.
    Usage(String),
    Run(Box<Options>),
}

const VALUED: [&str; 13] = [
    "host",
    "name",
    "revision",
    "identity",
    "inventory",
    "output",
    "user",
    "port",
    "known-hosts",
    "host-fingerprint",
    "password-file",
    "replacement-password-file",
    "become-password-file",
];
const REQUIRED: [&str; 6] = [
    "host",
    "name",
    "revision",
    "identity",
    "inventory",
    "output",
];

fn matches(pattern: &str, value: &str) -> bool {
    Regex::new(pattern).unwrap().is_match(value)
}

/// Reads the arguments after `bootstrap`. Selector values are checked after
/// the whole line is understood, each against its fixed shape.
pub(super) fn parse(arguments: &[String]) -> Result<Request> {
    let mut values = std::collections::BTreeMap::<&str, String>::new();
    let mut flags = std::collections::BTreeSet::<&str>::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if matches!(word.as_str(), "--help" | "-h") {
            return Ok(Request::Help);
        }
        let Some(option) = word.strip_prefix("--") else {
            return Ok(Request::Usage(format!("unexpected argument: {word}")));
        };
        let (name, inline) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (option, None),
        };
        let valued = VALUED
            .iter()
            .chain(&["minimum-free-gib"])
            .find(|known| **known == name);
        if let Some(known) = valued {
            let Some(value) = inline.or_else(|| words.next().cloned()) else {
                return Ok(Request::Usage(format!("--{name} needs a value")));
            };
            if values.insert(known, value).is_some() {
                return Ok(Request::Usage(format!("--{name} was given twice")));
            }
        } else if let Some(known) = ["provision", "initial-maintenance"]
            .iter()
            .find(|known| **known == name)
        {
            if inline.is_some() {
                return Ok(Request::Usage(format!("--{name} takes no value")));
            }
            flags.insert(known);
        } else {
            return Ok(Request::Usage(format!("unknown option: --{name}")));
        }
    }
    if let Some(missing) = REQUIRED.iter().find(|name| !values.contains_key(*name)) {
        return Ok(Request::Usage(format!("--{missing} is required")));
    }
    let number = |name: &str, default: u64| match values.get(name) {
        None => Some(default),
        Some(text) if matches("^[0-9]{1,9}$", text) => text.parse::<u64>().ok(),
        Some(_) => None,
    };
    let (Some(port), Some(minimum_free_gib)) = (number("port", 22), number("minimum-free-gib", 20))
    else {
        return Ok(Request::Usage(
            "--port and --minimum-free-gib take whole numbers".to_owned(),
        ));
    };
    let text = |name: &str, default: &str| values.get(name).cloned().unwrap_or(default.to_owned());
    let options = Options {
        host: text("host", ""),
        user: text("user", "debian"),
        port: u16::try_from(port).unwrap_or(0),
        name: text("name", ""),
        revision: text("revision", ""),
        identity: text("identity", ""),
        known_hosts: text("known-hosts", "~/.ssh/known_hosts"),
        host_fingerprint: values.get("host-fingerprint").cloned(),
        inventory: text("inventory", ""),
        output: text("output", ""),
        password_file: values.get("password-file").cloned(),
        replacement_password_file: values.get("replacement-password-file").cloned(),
        become_password_file: values.get("become-password-file").cloned(),
        minimum_free_gib,
        provision: flags.contains("provision"),
        initial_maintenance: flags.contains("initial-maintenance"),
    };
    require(
        matches(r"^[A-Za-z0-9][A-Za-z0-9.:-]{0,252}$", &options.host),
        "invalid_host",
    )?;
    require(
        matches(r"^[a-z_][a-z0-9_-]{0,31}$", &options.user),
        "invalid_user",
    )?;
    let alias = matches(r"^[A-Za-z][A-Za-z0-9_-]{0,62}$", &options.name);
    require(alias, "invalid_inventory_name")?;
    require(
        matches(r"^[a-f0-9]{40}$", &options.revision),
        "invalid_revision",
    )?;
    require(options.port >= 1, "invalid_port")?;
    require(
        (1..=1000).contains(&options.minimum_free_gib),
        "invalid_minimum_disk",
    )?;
    let maintained = !options.initial_maintenance || options.provision;
    require(maintained, "maintenance_requires_provision")?;
    let pinned = options.host_fingerprint.as_deref();
    let pinned = pinned.is_none_or(|value| matches(r"^SHA256:[A-Za-z0-9+/]{43}$", value));
    require(pinned, "invalid_host_fingerprint")?;
    Ok(Request::Run(Box::new(options)))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in super::super) const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// The required selectors, then `extra`.
    pub(in super::super) fn line(extra: &[&str]) -> Vec<String> {
        let required = [
            "--host",
            "test.example.invalid",
            "--name",
            "test_vps",
            "--revision",
            REVISION,
            "--identity",
            "/private/key",
            "--inventory",
            "ops/ansible/inventory.local.test.yml",
            "--output",
            "/private/evidence",
        ];
        required
            .iter()
            .chain(extra)
            .map(|word| (*word).to_owned())
            .collect()
    }

    fn run(extra: &[&str]) -> Options {
        match parse(&line(extra)).unwrap() {
            Request::Run(options) => *options,
            other => panic!("{other:?}"),
        }
    }

    /// Defaults, both value spellings and the two switches.
    #[test]
    fn selectors_have_the_documented_defaults_and_spellings() {
        let plain = run(&[]);
        assert_eq!((plain.user.as_str(), plain.port), ("debian", 22));
        assert_eq!(plain.known_hosts, "~/.ssh/known_hosts");
        assert_eq!(plain.minimum_free_gib, 20);
        assert!(!plain.provision && !plain.initial_maintenance);
        assert_eq!(plain.host_fingerprint, None);
        let fingerprint = format!("SHA256:{}", "a".repeat(43));
        let full = run(&[
            "--user=root",
            "--port",
            "2222",
            "--host-fingerprint",
            &fingerprint,
            "--password-file=/private/initial",
            "--replacement-password-file",
            "/private/replacement.json",
            "--become-password-file",
            "/private/sudo",
            "--minimum-free-gib=40",
            "--provision",
            "--initial-maintenance",
        ]);
        assert_eq!((full.user.as_str(), full.port), ("root", 2222));
        assert_eq!(full.host_fingerprint.as_deref(), Some(fingerprint.as_str()));
        assert_eq!(full.password_file.as_deref(), Some("/private/initial"));
        assert_eq!(full.minimum_free_gib, 40);
        assert!(full.provision && full.initial_maintenance);
        assert!(USAGE.contains("--host-fingerprint"));
    }

    /// Help wins; a line that is not an invocation is a usage error.
    #[test]
    fn help_and_usage_errors_are_not_refusals() {
        assert_eq!(parse(&line(&["--help"])).unwrap(), Request::Help);
        assert_eq!(parse(&["-h".to_owned()]).unwrap(), Request::Help);
        for (arguments, reason) in [
            (
                vec!["--host".to_owned(), "x".to_owned()],
                "--name is required",
            ),
            (line(&["--bogus"]), "unknown option: --bogus"),
            (line(&["stray"]), "unexpected argument: stray"),
            (line(&["--port"]), "--port needs a value"),
            (line(&["--host", "again"]), "--host was given twice"),
            (line(&["--provision=yes"]), "--provision takes no value"),
            (
                line(&["--port", "ssh"]),
                "--port and --minimum-free-gib take whole numbers",
            ),
        ] {
            assert_eq!(
                parse(&arguments).unwrap(),
                Request::Usage(reason.to_owned())
            );
        }
    }

    /// Every selector is checked against its fixed shape before use.
    #[test]
    fn malformed_selectors_are_refused_with_their_code() {
        let replaced = |flag: &str, value: &str| {
            let mut arguments = line(&[]);
            let at = arguments.iter().position(|word| word == flag).unwrap();
            arguments[at + 1] = value.to_owned();
            parse(&arguments).unwrap_err().0
        };
        assert_eq!(replaced("--host", "-oProxyCommand=x"), "invalid_host");
        assert_eq!(replaced("--host", "a b"), "invalid_host");
        assert_eq!(replaced("--name", "9lives"), "invalid_inventory_name");
        assert_eq!(replaced("--revision", "main"), "invalid_revision");
        let extra = |extra: &[&str]| parse(&line(extra)).unwrap_err().0;
        assert_eq!(extra(&["--user", "Root"]), "invalid_user");
        assert_eq!(extra(&["--port", "0"]), "invalid_port");
        assert_eq!(extra(&["--port", "65536"]), "invalid_port");
        assert_eq!(extra(&["--minimum-free-gib", "0"]), "invalid_minimum_disk");
        assert_eq!(
            extra(&["--minimum-free-gib", "1001"]),
            "invalid_minimum_disk"
        );
        assert_eq!(
            extra(&["--initial-maintenance"]),
            "maintenance_requires_provision"
        );
        assert_eq!(
            extra(&["--host-fingerprint", "MD5:aa"]),
            "invalid_host_fingerprint"
        );
    }
}
