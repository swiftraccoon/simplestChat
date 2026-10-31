//! Source scans for the tests that pin repository-wide rules on the server's
//! own code: which files and lines are production code, and the macro
//! invocations, function calls and enum variant constructions within them.

/// The text of a source file with the bodies of its inline test modules
/// blanked line for line, so each line keeps its number, or `None` when such a
/// body is never closed. An inline test module is `#[cfg(test)]`, then (past
/// any attributes, comments and blank lines) a line opening `mod name {`; its
/// body ends at the first later line holding that line's indentation and `}`,
/// followed by nothing but whitespace or a `//` comment, as rustfmt writes it,
/// so no brace inside the body is counted. A `mod name;` declaration has no
/// body here, so a file that declares its test modules above its own code is
/// still read to the end.
pub(crate) fn without_inline_test_modules(text: &str) -> Option<String> {
    let mut kept = String::new();
    let mut after_cfg_test = false;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let item = line.trim();
        let annotation = item.is_empty() || item.starts_with("#[") || item.starts_with("//");
        if after_cfg_test && !annotation {
            after_cfg_test = false;
            let mut words = item
                .split_whitespace()
                .skip_while(|word| word.starts_with("pub"));
            if words.next() == Some("mod") && item.ends_with('{') {
                let indent = &line[..line.len() - line.trim_start().len()];
                let close = format!("{indent}}}");
                let mut blanked = 1;
                lines.find(|later| {
                    blanked += 1;
                    later.strip_prefix(close.as_str()).is_some_and(|rest| {
                        rest.trim().is_empty() || rest.trim_start().starts_with("//")
                    })
                })?;
                kept.push_str(&"\n".repeat(blanked));
                continue;
            }
        }
        after_cfg_test |= item == "#[cfg(test)]";
        kept.push_str(line);
        kept.push('\n');
    }
    Some(kept)
}

/// A piece of Rust source, as coarse as the scans need. Whitespace and
/// comments are dropped and a literal is one piece, so a parenthesis inside a
/// string literal, a character literal or a comment is never counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    /// An identifier, keyword or number.
    Word(&'a str),
    /// The contents of a string literal (plain, byte or raw) as written,
    /// escapes included.
    Text(&'a str),
    /// Any other character outside a character literal.
    Mark(char),
}

/// One invocation of a function-like macro or construction of a tuple variant:
/// the line its name is on and the tokens between the parentheses around its
/// arguments.
#[derive(Debug)]
pub(crate) struct Invocation<'a> {
    pub(crate) line: usize,
    pub(crate) arguments: Vec<Token<'a>>,
}

/// Every `name!(…)` invocation in `text` of a macro in `names`, in source
/// order, through the parenthesis that closes its arguments; `None` when one
/// never closes. An invocation with brackets or braces is not collected.
pub(crate) fn macro_invocations<'a>(text: &'a str, names: &[&str]) -> Option<Vec<Invocation<'a>>> {
    let tokens = tokens(text);
    let mut invocations = Vec::new();
    for (start, window) in tokens.windows(3).enumerate() {
        let &[
            (line, Token::Word(name)),
            (_, Token::Mark('!')),
            (_, Token::Mark('(')),
        ] = window
        else {
            continue;
        };
        if names.contains(&name) {
            let arguments = arguments(&tokens[start + 3..])?;
            invocations.push(Invocation { line, arguments });
        }
    }
    Some(invocations)
}

/// Every `type_name::Variant(…)` construction in `text`, in source order,
/// through the parenthesis that closes its arguments; `None` when one never
/// closes.
pub(crate) fn variant_constructions<'a>(
    text: &'a str,
    type_name: &str,
) -> Option<Vec<Invocation<'a>>> {
    let tokens = tokens(text);
    let mut constructions = Vec::new();
    for (start, window) in tokens.windows(5).enumerate() {
        let &[
            (line, Token::Word(name)),
            (_, Token::Mark(':')),
            (_, Token::Mark(':')),
            (_, Token::Word(_)),
            (_, Token::Mark('(')),
        ] = window
        else {
            continue;
        };
        if name == type_name {
            let arguments = arguments(&tokens[start + 5..])?;
            constructions.push(Invocation { line, arguments });
        }
    }
    Some(constructions)
}

/// Every call `name(…)` in `text` of a function or method in `names`, in source
/// order, through the parenthesis that closes its arguments; `None` when one
/// never closes. A definition (`fn name(`) is not a call, nor is a macro.
pub(crate) fn calls<'a>(text: &'a str, names: &[&str]) -> Option<Vec<Invocation<'a>>> {
    let tokens = tokens(text);
    let mut calls = Vec::new();
    for (start, window) in tokens.windows(2).enumerate() {
        let &[(line, Token::Word(name)), (_, Token::Mark('('))] = window else {
            continue;
        };
        let definition = start
            .checked_sub(1)
            .is_some_and(|before| tokens[before].1 == Token::Word("fn"));
        if names.contains(&name) && !definition {
            let arguments = arguments(&tokens[start + 2..])?;
            calls.push(Invocation { line, arguments });
        }
    }
    Some(calls)
}

/// The tokens of an argument list whose opening parenthesis comes just before
/// `rest`, up to the parenthesis that closes it; `None` when none does.
fn arguments<'a>(rest: &[(usize, Token<'a>)]) -> Option<Vec<Token<'a>>> {
    let mut depth = 1;
    let mut arguments = Vec::new();
    for &(_, token) in rest {
        match token {
            Token::Mark('(') => depth += 1,
            Token::Mark(')') => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(arguments);
        }
        arguments.push(token);
    }
    None
}

/// The tokens of `text`, each with the line it starts on. A block comment ends
/// at the first `*/`: nested block comments are not supported, and the scanned
/// code has none.
fn tokens(text: &str) -> Vec<(usize, Token<'_>)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let (mut at, mut line) = (0, 1);
    while let Some(&byte) = bytes.get(at) {
        let rest = &text[at..];
        let (token, end) = if rest.starts_with("//") {
            (
                None,
                rest.find('\n').map_or(text.len(), |length| at + length),
            )
        } else if rest.starts_with("/*") {
            let close = rest.find("*/").map_or(text.len(), |length| at + length + 2);
            (None, close)
        } else if byte.is_ascii_whitespace() {
            (None, at + 1)
        } else if byte == b'"' {
            quoted(text, at + 1)
        } else if byte == b'\'' {
            character(text, at)
        } else if byte.is_ascii_alphanumeric() || byte == b'_' {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(text.len(), |length| at + length);
            let word = &text[at..end];
            match (word, bytes.get(end)) {
                ("r" | "br" | "cr", Some(b'"' | b'#')) => {
                    raw(text, end).unwrap_or((Some(Token::Word(word)), end))
                }
                ("b" | "c", Some(b'"')) => quoted(text, end + 1),
                _ => (Some(Token::Word(word)), end),
            }
        } else {
            let Some(mark) = rest.chars().next() else {
                break;
            };
            (Some(Token::Mark(mark)), at + mark.len_utf8())
        };
        if let Some(token) = token {
            tokens.push((line, token));
        }
        line += text[at..end].matches('\n').count();
        at = end;
    }
    tokens
}

/// A string literal whose contents start at `from`, past its opening quote:
/// the contents as written and the index past the closing quote, or the rest
/// of the text when the literal never closes.
fn quoted(text: &str, from: usize) -> (Option<Token<'_>>, usize) {
    let bytes = text.as_bytes();
    let mut at = from;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'\\' => at += 2,
            b'"' => return (Some(Token::Text(&text[from..at])), at + 1),
            _ => at += 1,
        }
    }
    (Some(Token::Text(&text[from..])), text.len())
}

/// A raw string literal whose `#`s or opening quote start at `from`: the
/// contents and the index past the closing quote and `#`s, or `None` for a raw
/// identifier such as `r#type`.
fn raw(text: &str, from: usize) -> Option<(Option<Token<'_>>, usize)> {
    let hashes = text[from..]
        .bytes()
        .take_while(|&byte| byte == b'#')
        .count();
    if text.as_bytes().get(from + hashes) != Some(&b'"') {
        return None;
    }
    let open = from + hashes + 1;
    let close = format!("\"{}", "#".repeat(hashes));
    Some(match text[open..].find(&close) {
        Some(length) => (
            Some(Token::Text(&text[open..open + length])),
            open + length + close.len(),
        ),
        None => (Some(Token::Text(&text[open..])), text.len()),
    })
}

/// A character literal whose quote is at `from`, which is skipped, or the
/// quote of a lifetime or a label, which stays a mark.
fn character(text: &str, from: usize) -> (Option<Token<'_>>, usize) {
    let rest = &text[from + 1..];
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        // An escape: past the escaped character to the closing quote, which
        // `\u{…}` puts further on.
        (Some('\\'), _) => {
            let close = rest.get(2..).and_then(|after| after.find('\''));
            (
                None,
                close.map_or(text.len(), |length| from + 3 + length + 1),
            )
        }
        (Some(first), Some('\'')) => (None, from + 1 + first.len_utf8() + 1),
        _ => (Some(Token::Mark('\'')), from + 1),
    }
}

/// The production text of every `.rs` file under `src/<directory>`, however
/// deep, with its path under `src`, in path order: `*_tests.rs` files and the
/// bodies of inline test modules are left out. Panics when a file cannot be
/// read or one of its test modules never closes.
pub(crate) fn production_sources(directory: &str) -> Vec<(String, String)> {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    let mut pending = vec![source.join(directory)];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path.to_string_lossy().ends_with("_tests.rs")
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path.strip_prefix(&source).unwrap().display().to_string();
            let text = std::fs::read_to_string(&path).unwrap();
            let production = without_inline_test_modules(&text)
                .unwrap_or_else(|| panic!("{name}: an inline test module is never closed"));
            (name, production)
        })
        .collect()
}

#[test]
fn reads_past_a_test_module_holding_a_brace_literal() {
    let source = r#"#[cfg(test)]
mod tests {
    #[test]
    fn opens_a_brace() {
        assert_eq!("{".len(), 1);
    }
}

fn after() {}
"#;
    let kept = without_inline_test_modules(source).expect("the module closes");
    assert!(
        kept.contains("fn after() {}"),
        "the code after the module is kept: {kept}"
    );
    assert!(
        !kept.contains("opens_a_brace"),
        "the module body is skipped: {kept}"
    );
}

#[test]
fn closes_a_test_module_on_a_commented_brace() {
    let source = r#"#[cfg(test)]
mod tests {
    #[test]
    fn closes() {}
} // mod tests

fn after() {}
"#;
    let kept = without_inline_test_modules(source).expect("the module closes");
    assert!(
        kept.contains("fn after() {}"),
        "the code after the comment is kept: {kept}"
    );
    assert!(
        !kept.contains("fn closes"),
        "the module body is skipped: {kept}"
    );
}

#[test]
fn fails_on_an_unclosed_test_module() {
    let source = r#"fn production() {}

#[cfg(test)]
mod tests {
    #[test]
    fn unclosed() {}
"#;
    assert_eq!(
        without_inline_test_modules(source),
        None,
        "an unclosed test module fails the scan"
    );
}

#[test]
fn reads_past_a_declared_test_module() {
    let source = r#"#[cfg(test)]
#[path = "port_tests.rs"]
mod tests;

fn after() {}
"#;
    let kept = without_inline_test_modules(source).expect("nothing to close");
    assert!(
        kept.contains("fn after() {}"),
        "the code after the declaration is kept: {kept}"
    );
}

#[test]
fn reads_test_only_functions() {
    let source = r#"#[cfg(test)]
fn fixture() -> u16 {
    3000
}
"#;
    let kept = without_inline_test_modules(source).expect("nothing to close");
    assert!(
        kept.contains("3000"),
        "a #[cfg(test)] function is kept: {kept}"
    );
}

#[test]
fn skips_the_body_of_an_inline_test_module() {
    let source = r#"fn production() {}

#[cfg(test)]
mod tests {
    #[test]
    fn database() {
        let _ = inside();
    }
}
"#;
    let kept = without_inline_test_modules(source).expect("the module closes");
    assert!(
        kept.contains("fn production() {}"),
        "the production code is kept: {kept}"
    );
    assert!(
        !kept.contains("inside()"),
        "the module body is skipped: {kept}"
    );
}

#[test]
fn blanks_a_test_module_line_for_line() {
    let source = r#"fn first() {}
#[cfg(test)]
mod tests {
    fn inside() {}
} // mod tests
fn last() {}
"#;
    let kept = without_inline_test_modules(source).expect("the module closes");
    assert_eq!(
        kept.lines().collect::<Vec<_>>(),
        ["fn first() {}", "#[cfg(test)]", "", "", "", "fn last() {}"],
        "each line keeps its number"
    );
}

#[test]
fn macro_invocations_close_on_their_own_parenthesis() {
    use Token::{Mark, Text, Word};
    let source = r##"fn report() {
    info!("a ) in text", count(1), ')');
    // info!(commented out
    /* warn!( also out */
    warn!(
        r#"raw ")" text"#,
        "escaped \" quote )",
        'x',
    );
    xinfo!(not, collected);
}
"##;
    let found = macro_invocations(source, &["info", "warn"]).expect("every invocation closes");
    let found: Vec<_> = found
        .iter()
        .map(|invocation| (invocation.line, invocation.arguments.as_slice()))
        .collect();
    assert_eq!(
        found,
        [
            (
                2,
                &[
                    Text("a ) in text"),
                    Mark(','),
                    Word("count"),
                    Mark('('),
                    Word("1"),
                    Mark(')'),
                    Mark(','),
                ][..]
            ),
            (
                5,
                &[
                    Text("raw \")\" text"),
                    Mark(','),
                    Text("escaped \\\" quote )"),
                    Mark(','),
                    Mark(','),
                ][..]
            ),
        ]
    );
}

#[test]
fn macro_invocations_refuse_one_that_never_closes() {
    let source = "fn report() {\n    info!(\"open\";\n}\n";
    assert!(macro_invocations(source, &["info"]).is_none());
}

#[test]
fn variant_constructions_close_on_their_own_parenthesis() {
    use Token::{Mark, Text, Word};
    let source = r#"fn fail(error: anyhow::Error) -> Result<(), MediaError> {
    let _ = MediaError::RouterError(format!("no router (for {id})"));
    Err(crate::media::MediaError::Other(error.into()))
}
"#;
    let found = variant_constructions(source, "MediaError").expect("every construction closes");
    let found: Vec<_> = found
        .iter()
        .map(|construction| (construction.line, construction.arguments.as_slice()))
        .collect();
    assert_eq!(
        found,
        [
            (
                2,
                &[
                    Word("format"),
                    Mark('!'),
                    Mark('('),
                    Text("no router (for {id})"),
                    Mark(')'),
                ][..]
            ),
            (
                3,
                &[Word("error"), Mark('.'), Word("into"), Mark('('), Mark(')')][..]
            ),
        ]
    );
}

#[test]
fn calls_skip_definitions_and_macros() {
    use Token::{Mark, Text, Word};
    let source = r#"fn not_found(message: &str) {}
let missing = not_found("gone");
let failure = read().context(format!("x"));
let macro_call = not_found!("not a call");
"#;
    let found = calls(source, &["not_found", "context"]).expect("every call closes");
    let found: Vec<_> = found
        .iter()
        .map(|call| (call.line, call.arguments.as_slice()))
        .collect();
    assert_eq!(
        found,
        [
            (2, &[Text("gone")][..]),
            (
                3,
                &[Word("format"), Mark('!'), Mark('('), Text("x"), Mark(')')][..]
            ),
        ]
    );
}

#[test]
fn production_sources_walk_subdirectories_without_test_files() {
    let names: Vec<String> = production_sources("room")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for expected in [
        "room/mod.rs",
        "room/manager/lobby.rs",
        "room/social/members.rs",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected}: {names:?}"
        );
    }
    assert!(
        names.iter().all(|name| !name.ends_with("_tests.rs")),
        "{names:?}"
    );
}
