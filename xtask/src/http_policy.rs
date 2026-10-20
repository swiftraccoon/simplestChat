//! `http-policy`: write `security/http-policy.json` from the server's content
//! security policy and fixed security headers, so the release fixture's
//! response check reads the one definition the server sends. `--check` fails
//! when the committed file is stale, like `protocol-ts`.

use anyhow::{Context, Result, bail};
use std::path::Path;

const SOURCE: &str = "src/signaling/mod.rs";
const OUTPUT: &str = "security/http-policy.json";

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<()> {
    let check = match arguments {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => bail!("http-policy accepts only --check"),
    };
    let generated = generate(root)?;
    let output = root.join(OUTPUT);
    if check {
        let current = std::fs::read_to_string(&output)
            .with_context(|| format!("reading {}", output.display()))?;
        if current != generated {
            bail!("{OUTPUT} is stale; run `cargo run -q -p xtask -- http-policy`");
        }
        println!("http-policy: {OUTPUT} is up to date");
        return Ok(());
    }
    std::fs::write(&output, generated).with_context(|| format!("writing {}", output.display()))?;
    println!("http-policy: wrote {OUTPUT}");
    Ok(())
}

fn generate(root: &Path) -> Result<String> {
    let text =
        std::fs::read_to_string(root.join(SOURCE)).with_context(|| format!("reading {SOURCE}"))?;
    let file = syn::parse_file(&text).with_context(|| format!("parsing {SOURCE}"))?;
    let mut policy = None;
    let mut headers = None;
    for item in &file.items {
        let syn::Item::Const(item) = item else {
            continue;
        };
        if item.ident == "CONTENT_SECURITY_POLICY" {
            policy = Some(string_literal(&item.expr).context("CONTENT_SECURITY_POLICY")?);
        } else if item.ident == "SECURITY_HEADERS" {
            headers = Some(header_pairs(&item.expr).context("SECURITY_HEADERS")?);
        }
    }
    let policy = policy.with_context(|| format!("no CONTENT_SECURITY_POLICY in {SOURCE}"))?;
    let headers = headers.with_context(|| format!("no SECURITY_HEADERS in {SOURCE}"))?;
    let mut directives = serde_json::Map::new();
    for directive in policy
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (name, value) = directive
            .split_once(' ')
            .with_context(|| format!("directive `{directive}` has no value"))?;
        if directives
            .insert(
                name.to_owned(),
                serde_json::Value::String(value.trim().to_owned()),
            )
            .is_some()
        {
            bail!("directive `{name}` repeats");
        }
    }
    let mut fixed = serde_json::Map::new();
    for (name, value) in headers {
        if fixed
            .insert(name.clone(), serde_json::Value::String(value))
            .is_some()
        {
            bail!("header `{name}` repeats");
        }
    }
    let document = serde_json::json!({
        "contentSecurityPolicy": policy,
        "directives": directives,
        "headers": fixed,
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&document)?))
}

fn string_literal(expr: &syn::Expr) -> Result<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) => Ok(text.value()),
        _ => bail!("expected a string literal"),
    }
}

/// `[("name", "value"), …]`: the names lowercase, as the wire carries them.
fn header_pairs(expr: &syn::Expr) -> Result<Vec<(String, String)>> {
    let syn::Expr::Array(array) = expr else {
        bail!("expected an array of (name, value) pairs");
    };
    array
        .elems
        .iter()
        .map(|element| {
            let syn::Expr::Tuple(pair) = element else {
                bail!("expected a (name, value) tuple");
            };
            let [name, value] = pair.elems.iter().collect::<Vec<_>>()[..] else {
                bail!("expected exactly a name and a value");
            };
            let name = string_literal(name)?;
            if name != name.to_ascii_lowercase() {
                bail!("header name `{name}` is not lowercase");
            }
            Ok((name, string_literal(value)?))
        })
        .collect()
}
