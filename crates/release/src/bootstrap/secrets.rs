//! Passwords: read from a protected file outside the checkout or from a
//! terminal prompt, never from arguments or the environment.

use super::files::{metadata, outside_checkout, protected_file, write_record};
use super::{Refusal, Result, require};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr};
use serde_json::{Value, json};
use std::fmt;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::Path;

/// The longest password accepted, in characters.
const MAX_PASSWORD_CHARACTERS: usize = 1024;

/// A password held only in process memory; it has no printable form.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Password(String);

impl Password {
    /// The secret itself, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Password(..)")
    }
}

/// Where a password is typed when no file supplies it.
pub(super) trait Terminal {
    /// Shows `prompt` and reads one line without echoing it.
    fn password(&self, prompt: &str) -> Result<String>;
}

/// The controller's own terminal.
pub(super) struct Console;

/// Restores the terminal's modes when the prompt ends, however it ends.
struct Restore(Termios);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = tcsetattr(std::io::stdin(), OptionalActions::Flush, &self.0);
    }
}

impl Terminal for Console {
    fn password(&self, prompt: &str) -> Result<String> {
        let input = std::io::stdin();
        require(input.is_terminal(), "password_prompt_requires_terminal")?;
        let original = tcgetattr(&input).map_err(metadata)?;
        let mut silent = original.clone();
        silent.local_modes.remove(LocalModes::ECHO);
        let restore = Restore(original);
        tcsetattr(&input, OptionalActions::Flush, &silent).map_err(metadata)?;
        let mut screen = std::io::stderr();
        screen.write_all(prompt.as_bytes()).map_err(metadata)?;
        screen.flush().map_err(metadata)?;
        let mut line = String::new();
        input.lock().read_line(&mut line).map_err(metadata)?;
        drop(restore);
        screen.write_all(b"\n").map_err(metadata)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    }
}

/// A plain password, or the `password` field of a private JSON object, from
/// `path`; without a path, one typed at `terminal`.
pub(super) fn password_value(
    path: Option<&Path>,
    prompt: &str,
    root: &Path,
    terminal: &dyn Terminal,
) -> Result<Password> {
    let value = match path {
        None => terminal.password(prompt)?,
        Some(path) => {
            outside_checkout(path, root)?;
            let text = String::from_utf8(protected_file(path, true)?).map_err(metadata)?;
            if text.trim_start().starts_with('{') {
                let decoded: Value =
                    serde_json::from_str(&text).map_err(|_| Refusal("invalid_password_file"))?;
                let password = decoded
                    .as_object()
                    .and_then(|object| object.get("password"));
                let password = password.and_then(Value::as_str);
                password.ok_or(Refusal("invalid_password_file"))?.to_owned()
            } else {
                text.trim_end_matches(['\r', '\n']).to_owned()
            }
        }
    };
    let length = value.chars().count();
    let bounded = (1..=MAX_PASSWORD_CHARACTERS).contains(&length);
    require(bounded, "invalid_password_length")?;
    let plain = !value.contains(['\0', '\r', '\n']);
    require(plain, "invalid_password_characters")?;
    Ok(Password(value))
}

/// The replacement password, saved before SSH is contacted so that an
/// interrupted rotation stays recoverable. An existing file is reused as is.
pub(super) fn replacement_password(
    path: &Path,
    root: &Path,
    terminal: &dyn Terminal,
) -> Result<Password> {
    outside_checkout(path, root)?;
    if path.symlink_metadata().is_ok() {
        return password_value(Some(path), "", root, terminal);
    }
    let mut random = [0u8; 36];
    let mut source = std::fs::File::open("/dev/urandom").map_err(metadata)?;
    source.read_exact(&mut random).map_err(metadata)?;
    let value = URL_SAFE_NO_PAD.encode(random);
    write_record(path, &json!({"password": value}))?;
    Ok(Password(value))
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::files::tests::Scratch;
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// A terminal that types one fixed answer, or refuses like a pipe. The
    /// answer is built at run time: no password is spelled in the source.
    pub(in super::super) struct Typed(pub Option<String>);

    impl Terminal for Typed {
        fn password(&self, _: &str) -> Result<String> {
            let typed = self.0.as_ref();
            Ok(typed
                .ok_or(Refusal("password_prompt_requires_terminal"))?
                .clone())
        }
    }

    fn code(result: Result<Password>) -> &'static str {
        result.unwrap_err().0
    }

    /// Plain and JSON password files, the terminal, and every refusal.
    #[test]
    fn passwords_come_from_protected_files_or_the_terminal_only() {
        let private = Scratch::new();
        let checkout = Scratch::new();
        let none = Typed(None);
        let read = |path: &Path| password_value(Some(path), "", &checkout.0, &none);
        let plain = private.file("plain", b"first secret\r\n", 0o600);
        assert_eq!(read(&plain).unwrap().expose(), "first secret");
        let object = private.file("object", b" {\"password\": \"from json\"}\n", 0o600);
        assert_eq!(read(&object).unwrap().expose(), "from json");
        let answer = "t".repeat(9);
        let typed = Typed(Some(answer.clone()));
        let typed = password_value(None, "Initial SSH password: ", &checkout.0, &typed);
        assert_eq!(typed.unwrap().expose(), answer);
        let piped = password_value(None, "Initial SSH password: ", &checkout.0, &none);
        assert_eq!(code(piped), "password_prompt_requires_terminal");
        for (name, bytes, expected) in [
            ("number", &b"{\"password\": 7}"[..], "invalid_password_file"),
            ("list", b"{\"password\": [\"x\"]}", "invalid_password_file"),
            ("broken", b"{\"password\": ", "invalid_password_file"),
            ("blank", b"\n", "invalid_password_length"),
            ("lines", b"one\ntwo\n", "invalid_password_characters"),
            ("nul", b"one\0two", "invalid_password_characters"),
        ] {
            let path = private.file(name, bytes, 0o600);
            assert_eq!(code(read(&path)), expected, "{name}");
        }
        let long = "x".repeat(MAX_PASSWORD_CHARACTERS + 1);
        let long = private.file("long", long.as_bytes(), 0o600);
        assert_eq!(code(read(&long)), "invalid_password_length");
        let readable = private.file("readable", b"secret\n", 0o644);
        assert_eq!(code(read(&readable)), "invalid_private_file_mode");
        let inside = checkout.file("inside", b"secret\n", 0o600);
        assert_eq!(code(read(&inside)), "credential_path_inside_checkout");
        let shown = format!("{:?}", read(&plain).unwrap());
        assert_eq!(shown, "Password(..)");
    }

    /// From `test_replacement_is_saved_exclusively_and_reused`: recovery
    /// credentials exist before SSH and a retry never replaces them.
    #[test]
    fn replacement_is_saved_exclusively_and_reused() {
        let private = Scratch::new();
        let checkout = Scratch::new();
        let path = private.0.join("replacement.json");
        let first = replacement_password(&path, &checkout.0, &Typed(None)).unwrap();
        assert_eq!(first.expose().len(), 48);
        assert!(
            first
                .expose()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        );
        assert_eq!(path.metadata().unwrap().mode() & 0o7777, 0o600);
        let original = std::fs::read(&path).unwrap();
        let again = replacement_password(&path, &checkout.0, &Typed(None)).unwrap();
        assert_eq!(again, first);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let other = private.0.join("other.json");
        let second = replacement_password(&other, &checkout.0, &Typed(None)).unwrap();
        assert_ne!(second, first);
        let inside = replacement_password(&checkout.0.join("r.json"), &checkout.0, &Typed(None));
        assert_eq!(code(inside), "credential_path_inside_checkout");
    }
}
