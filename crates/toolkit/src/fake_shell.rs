//! POSIX `sh` functions shared by the workspace tests' fake commands, built
//! only for this crate's tests and the `test-hooks` feature. Each fake is a
//! `#!/bin/sh` script that starts with [`PRELUDE`] and calls these functions
//! to record what it saw; they need no program outside the shell.

/// Shell functions for a fake's records and outputs:
///
/// - `json_string VALUE` prints VALUE as a JSON string. The quote and the
///   backslash are escaped, every control character becomes `\uXXXX`, and
///   other bytes pass through unchanged.
/// - `json_strings VALUE…` prints a JSON array of strings, separated by
///   `", "`.
/// - `json_value NAME` prints the variable NAME as a JSON string, or `null`
///   when it is unset.
/// - `repeat TEXT COUNT` prints TEXT COUNT times.
pub const PRELUDE: &str = r#"json_string() {
  json_rest=$1 json_out=
  while [ -n "$json_rest" ]; do
    json_tail=${json_rest#?}
    json_char=${json_rest%"$json_tail"}
    json_rest=$json_tail
    case $json_char in
      '"' | '\') json_out="$json_out\\$json_char" ;;
      [[:cntrl:]]) json_out="$json_out$(printf '\\u%04x' "'$json_char")" ;;
      *) json_out="$json_out$json_char" ;;
    esac
  done
  printf '"%s"' "$json_out"
}
json_strings() {
  printf '['
  json_separator=
  for json_item in "$@"; do
    printf '%s' "$json_separator"
    json_string "$json_item"
    json_separator=', '
  done
  printf ']'
}
json_value() {
  if eval "[ \"\${$1+set}\" = set ]"; then
    eval "json_string \"\$$1\""
  else
    printf null
  fi
}
repeat() {
  repeat_unit=$1 repeat_count=$2 repeat_out=
  while [ "$repeat_count" -gt 0 ]; do
    if [ $((repeat_count % 2)) -eq 1 ]; then
      repeat_out=$repeat_out$repeat_unit
    fi
    repeat_unit=$repeat_unit$repeat_unit
    repeat_count=$((repeat_count / 2))
  done
  printf '%s' "$repeat_out"
}
"#;

/// `value` as JSON text in the spacing and escaping a fake's scripted answers
/// use: `", "` between items, `": "` after a key, and ASCII only, with the
/// quote, the backslash, every character outside `' '..='~'` and every
/// non-ASCII character escaped (`\n`, `\r`, `\t`, `\b`, `\f`, else
/// `\uXXXX`, a surrogate pair above U+FFFF). Object members keep the map's
/// order.
#[must_use]
pub fn spaced_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => {
            let mut escaped = String::from("\"");
            for character in text.chars() {
                match character {
                    '"' => escaped.push_str("\\\""),
                    '\\' => escaped.push_str("\\\\"),
                    '\n' => escaped.push_str("\\n"),
                    '\r' => escaped.push_str("\\r"),
                    '\t' => escaped.push_str("\\t"),
                    '\u{8}' => escaped.push_str("\\b"),
                    '\u{c}' => escaped.push_str("\\f"),
                    ' '..='~' => escaped.push(character),
                    _ => {
                        let mut units = [0_u16; 2];
                        for unit in character.encode_utf16(&mut units) {
                            escaped.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                }
            }
            escaped.push('"');
            escaped
        }
        Value::Array(items) => {
            let items = items.iter().map(spaced_json).collect::<Vec<_>>();
            format!("[{}]", items.join(", "))
        }
        Value::Object(members) => {
            let members = members
                .iter()
                .map(|(key, item)| {
                    let key = spaced_json(&Value::String(key.clone()));
                    format!("{key}: {}", spaced_json(item))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", members.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PRELUDE, spaced_json};
    use std::process::{Command, Stdio};

    /// Spaced JSON parses back to the same value, stays ASCII, and spaces
    /// its separators.
    #[test]
    fn spaced_json_is_ascii_json_of_the_same_value() {
        let value = serde_json::json!({
            "list": [1, 0.0, true, null],
            "text": "é and 😀 and \u{7f} and \"\\\n\t\u{1}",
            "empty": {}
        });
        let text = spaced_json(&value);
        assert!(text.is_ascii());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            value
        );
        assert!(text.contains("[1, 0.0, true, null]"));
        assert!(text.contains("\\ud83d\\ude00") && text.contains("\\u007f"));
        assert!(text.contains("\"empty\": {}") && text.contains("\"list\": [1, "));
    }

    /// What the prelude prints for `script`, run by `shell`.
    fn printed(shell: &str, script: &str, arguments: &[&str]) -> Vec<u8> {
        let output = Command::new(shell)
            .arg("-c")
            .arg(format!("{PRELUDE}{script}"))
            .arg("fake")
            .args(arguments)
            .env_clear()
            .env("SET_EMPTY", "")
            .env("SET_TEXT", "a \"quoted\" λ\\path")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{shell}");
        output.stdout
    }

    /// JSON text round-trips through a JSON parser to the original values,
    /// in each shell the fakes run under.
    #[test]
    fn records_are_json_with_python_separators_in_each_shell() {
        let values = [
            "plain",
            "",
            "quote \" and \\ slash",
            "tab\tnew\nline\u{1}",
            "λ $(x) *",
        ];
        for shell in ["/bin/sh", "/bin/dash"] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            let array = printed(shell, r#"json_strings "$@""#, &values);
            let parsed: Vec<String> = serde_json::from_slice(&array).unwrap();
            assert_eq!(parsed, values, "{shell}");
            assert!(array.starts_with(b"[\"plain\", \"\", "), "{shell}");
            let unset = printed(shell, "json_value NOT_SET", &[]);
            assert_eq!(unset, b"null", "{shell}");
            let empty = printed(shell, "json_value SET_EMPTY", &[]);
            assert_eq!(empty, b"\"\"", "{shell}");
            let text = printed(shell, "json_value SET_TEXT", &[]);
            let text: String = serde_json::from_slice(&text).unwrap();
            assert_eq!(text, "a \"quoted\" λ\\path", "{shell}");
            for count in [0, 1, 7, 4097] {
                let repeated = printed(shell, r#"repeat ab "$1""#, &[&count.to_string()]);
                assert_eq!(repeated, "ab".repeat(count).as_bytes(), "{shell} {count}");
            }
        }
    }
}
