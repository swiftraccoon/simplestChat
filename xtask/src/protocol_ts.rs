//! `protocol-ts`: emit `web/src/protocol.generated.ts` from the Rust wire types.
//!
//! The Rust definitions are the contract. This command parses the modules that
//! declare wire types with `syn`, applies the serde attributes the server uses
//! (`rename_all`, `rename_all_fields`, `tag`/`content`, `flatten`, `default`,
//! `skip_serializing_if`, `transparent`) and writes the TypeScript the web client imports. `--check`
//! fails when the committed file is stale, so CI keeps both sides equal.
//!
//! Optionality follows the client's decoder, not the raw JSON: a server
//! optional (`Option<T>`) becomes `name?: T` because the decoder normalizes
//! `null` and absent values alike, and a client optional is sent or omitted.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// Files that declare types the wire mentions, relative to the repository root.
const SOURCES: &[&str] = &[
    "src/signaling/protocol.rs",
    "src/room/roles.rs",
    "src/room/settings.rs",
    "src/room/history.rs",
    "src/room/moderation.rs",
    "src/turn.rs",
    "src/auth/account.rs",
    "src/auth/types.rs",
    "src/room/api.rs",
];

/// Rust type names whose TypeScript spelling differs from the Rust one.
const RENAMES: &[(&str, &str)] = &[
    ("HistoryPage", "ChatHistoryPage"),
    ("ModerationEventView", "ModerationEventEntry"),
    ("IceServer", "IceServerEntry"),
];

/// Rust types that serialize as plain strings.
const STRING_TYPES: &[&str] = &[
    "String",
    "str",
    "SecretString",
    "AuthenticationToken",
    "Uuid",
    "DateTime",
    "RoomId",
    "ParticipantId",
    "MessageId",
    "UserId",
    "TransportId",
    "ProducerId",
    "ConsumerId",
    "ModerationTargetId",
    "IpAddr",
];

const NUMBER_TYPES: &[&str] = &[
    "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "f32", "f64",
];

/// Client library contracts the wire reuses; imported, never generated.
const MEDIASOUP_TYPES: &[&str] = &[
    "DtlsParameters",
    "RtpParameters",
    "RtpCapabilities",
    "RtpCapabilitiesFinalized",
    "IceParameters",
    "IceCandidate",
];

/// Limits the browser applies before the server does: each a `const` with an
/// integer literal in the named source, emitted in this order with its doc comment.
const CONSTANTS: &[(&str, &str)] = &[
    ("src/auth/password_policy.rs", "MIN_PASSWORD_CHARS"),
    ("src/auth/password_policy.rs", "MAX_PASSWORD_CHARS"),
    ("src/auth/password_policy.rs", "MAX_PASSWORD_BYTES"),
    ("src/auth/routes.rs", "MAX_DISPLAY_NAME_LEN"),
];

const OUTPUT: &str = "web/src/protocol.generated.ts";

/// Mapped types over the generated action maps; fixed text because they are
/// TypeScript-level conveniences, not wire shapes.
const SOCIAL_HELPERS: &str = "\
export type SocialRequest = {
  [A in SocialAction]: { type: A; requestId: string } & (SocialRequests[A] extends undefined
    ? object
    : SocialRequests[A]);
}[SocialAction];
/** No-payload/page actions may omit data; mutation payloads remain required. */
export type SocialRequestArguments<A extends SocialAction> = {
  [K in A]: SocialRequests[K] extends undefined
    ? [action: K]
    : object extends SocialRequests[K]
      ? [action: K, data?: SocialRequests[K]]
      : [action: K, data: SocialRequests[K]];
}[A];
";

pub fn run(root: &Path, arguments: &[String]) -> Result<()> {
    let check = match arguments {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => bail!("protocol-ts accepts only --check"),
    };
    let generated = generate(root)?;
    let output = root.join(OUTPUT);
    if check {
        let current = std::fs::read_to_string(&output)
            .with_context(|| format!("reading {}", output.display()))?;
        if current != generated {
            bail!("{OUTPUT} is stale; run `cargo run -q -p xtask -- protocol-ts`");
        }
        println!("protocol-ts: {OUTPUT} is up to date");
        return Ok(());
    }
    std::fs::write(&output, generated).with_context(|| format!("writing {}", output.display()))?;
    println!("protocol-ts: wrote {OUTPUT}");
    Ok(())
}

#[derive(Clone, Debug, Default)]
struct SerdeAttributes {
    rename_all: Option<String>,
    rename_all_fields: Option<String>,
    rename: Option<String>,
    tag: Option<String>,
    content: Option<String>,
    flatten: bool,
    default: bool,
    skip_serializing_if: bool,
    skip: bool,
    transparent: bool,
    untagged: bool,
    skip_serializing: bool,
    skip_deserializing: bool,
}

fn serde_attributes(attrs: &[syn::Attribute]) -> Result<SerdeAttributes> {
    let mut found = SerdeAttributes::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            let name = meta
                .path
                .get_ident()
                .map(ToString::to_string)
                .unwrap_or_default();
            match name.as_str() {
                "rename_all" => found.rename_all = Some(string_value(&meta)?),
                "rename_all_fields" => found.rename_all_fields = Some(string_value(&meta)?),
                "rename" => found.rename = Some(string_value(&meta)?),
                "tag" => found.tag = Some(string_value(&meta)?),
                "content" => found.content = Some(string_value(&meta)?),
                "skip_serializing_if" => {
                    let _ = string_value(&meta)?;
                    found.skip_serializing_if = true;
                }
                "try_from" | "into" | "from" | "with" | "deserialize_with" | "serialize_with" => {
                    let _ = string_value(&meta)?;
                }
                "flatten" => found.flatten = true,
                "default" => {
                    found.default = true;
                    if meta.input.peek(syn::Token![=]) {
                        let _ = string_value(&meta)?;
                    }
                }
                "skip" => found.skip = true,
                "skip_deserializing" => found.skip_deserializing = true,
                "transparent" => found.transparent = true,
                "untagged" => found.untagged = true,
                "skip_serializing" => found.skip_serializing = true,
                "deny_unknown_fields" | "other" => {}
                other => {
                    return Err(meta.error(format!("unsupported serde attribute `{other}`")));
                }
            }
            Ok(())
        })
        .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(found)
}

fn string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value: syn::LitStr = meta.value()?.parse()?;
    Ok(value.value())
}

fn rename(name: &str, rule: Option<&str>) -> String {
    match rule {
        Some("camelCase") => camel_case(name),
        Some("snake_case") => snake_case(name),
        Some("lowercase") => name.to_lowercase(),
        Some("SCREAMING_SNAKE_CASE") => snake_case(name).to_uppercase(),
        Some("kebab-case") => snake_case(name).replace('_', "-"),
        Some(other) => panic!("unsupported rename_all `{other}`"),
        None => name.to_owned(),
    }
}

/// `snake_case` field or `PascalCase` variant → `camelCase`.
fn camel_case(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for (index, ch) in name.chars().enumerate() {
        if ch == '_' {
            upper = true;
            continue;
        }
        if index == 0 {
            out.extend(ch.to_lowercase());
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// `PascalCase` → `snake_case`.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

enum Definition {
    Struct(syn::ItemStruct),
    Enum(syn::ItemEnum),
}

struct Generator {
    definitions: BTreeMap<String, Definition>,
    /// Types discovered while rendering, with the direction they were reached from.
    emitted: BTreeMap<String, bool>,
    used_mediasoup: BTreeSet<String>,
    needs_media_kind: bool,
    /// Whether the type being rendered is sent by the client (defaults may be omitted).
    client_sent: bool,
    /// Adjacently tagged enums flattened into a tagged frame, with that frame's own fields.
    envelopes: BTreeMap<String, Vec<String>>,
}

impl Generator {
    fn ts_name(name: &str) -> String {
        RENAMES
            .iter()
            .find(|(rust, _)| *rust == name)
            .map_or_else(|| name.to_owned(), |(_, ts)| (*ts).to_owned())
    }

    fn last_segment(path: &syn::TypePath) -> Result<(String, Vec<syn::Type>)> {
        let segment = path
            .path
            .segments
            .last()
            .context("type path without segments")?;
        let arguments = match &segment.arguments {
            syn::PathArguments::None => Vec::new(),
            syn::PathArguments::AngleBracketed(generic) => generic
                .args
                .iter()
                .filter_map(|argument| match argument {
                    syn::GenericArgument::Type(ty) => Some(ty.clone()),
                    _ => None,
                })
                .collect(),
            syn::PathArguments::Parenthesized(_) => bail!("function types are not wire types"),
        };
        Ok((segment.ident.to_string(), arguments))
    }

    /// The TypeScript for a Rust type, after unwrapping `Option` (reported separately).
    fn ts_type(&mut self, ty: &syn::Type) -> Result<(String, bool)> {
        match ty {
            syn::Type::Path(path) => {
                let (name, arguments) = Self::last_segment(path)?;
                match name.as_str() {
                    "Option" => {
                        let inner = arguments.first().context("Option without argument")?;
                        let (text, nested) = self.ts_type(inner)?;
                        // `Option<Option<T>>` distinguishes "leave unchanged" from "clear".
                        let text = if nested {
                            format!("{text} | null")
                        } else {
                            text
                        };
                        Ok((text, true))
                    }
                    "Vec" | "VecDeque" | "BTreeSet" | "HashSet" => {
                        let inner = arguments.first().context("sequence without argument")?;
                        let (text, _) = self.ts_type(inner)?;
                        let wrapped = if text.contains(' ') {
                            format!("({text})[]")
                        } else {
                            format!("{text}[]")
                        };
                        Ok((wrapped, false))
                    }
                    "HashMap" | "BTreeMap" => {
                        let value = arguments.get(1).context("map without value type")?;
                        let (text, _) = self.ts_type(value)?;
                        Ok((format!("Record<string, {text}>"), false))
                    }
                    "Box" | "Arc" | "Rc" => {
                        let inner = arguments.first().context("box without argument")?;
                        self.ts_type(inner)
                    }
                    "Value" => Ok(("unknown".to_owned(), false)),
                    "bool" => Ok(("boolean".to_owned(), false)),
                    "MediaKind" => {
                        self.needs_media_kind = true;
                        Ok(("MediaKind".to_owned(), false))
                    }
                    other if STRING_TYPES.contains(&other) => Ok(("string".to_owned(), false)),
                    other if NUMBER_TYPES.contains(&other) => Ok(("number".to_owned(), false)),
                    other if MEDIASOUP_TYPES.contains(&other) => {
                        self.used_mediasoup.insert(other.to_owned());
                        Ok((other.to_owned(), false))
                    }
                    other => {
                        if !self.definitions.contains_key(other) {
                            bail!("no wire definition for type `{other}`");
                        }
                        self.emit_definition(other)?;
                        Ok((Self::ts_name(other), false))
                    }
                }
            }
            syn::Type::Reference(reference) => self.ts_type(&reference.elem),
            syn::Type::Tuple(tuple) if tuple.elems.is_empty() => Ok(("null".to_owned(), false)),
            other => bail!("unsupported type syntax `{}`", quote_type(other)),
        }
    }

    fn fields_text(
        &mut self,
        fields: &syn::FieldsNamed,
        rename_all: Option<&str>,
    ) -> Result<(Vec<FieldText>, Vec<String>)> {
        let mut lines = Vec::new();
        let mut flattened = Vec::new();
        for field in &fields.named {
            let attrs = serde_attributes(&field.attrs)?;
            // A skipped field never reaches the wire; one skipped only on output never
            // reaches the client, and one skipped only on input is never read from it.
            if attrs.skip
                || (attrs.skip_serializing && !self.client_sent)
                || (attrs.skip_deserializing && self.client_sent)
            {
                continue;
            }
            let ident = field.ident.as_ref().context("named field")?.to_string();
            if attrs.flatten {
                let (text, _) = self.ts_type(&field.ty)?;
                flattened.push(text);
                continue;
            }
            let name = attrs
                .rename
                .clone()
                .unwrap_or_else(|| rename(&ident, rename_all));
            let (text, optional) = self.ts_type(&field.ty)?;
            // The wire shape: a skipped optional is absent and a plain server optional
            // is null; a client may leave any optional or defaulted field out, because
            // serde reads a missing `Option` as `None`.
            let omitted =
                attrs.skip_serializing_if || (self.client_sent && (attrs.default || optional));
            let (marker, text) = if omitted {
                ("?", text)
            } else if optional {
                ("", format!("{text} | null"))
            } else {
                ("", text)
            };
            lines.push(FieldText {
                docs: doc_lines(&field.attrs),
                text: format!("{name}{marker}: {text};"),
            });
        }
        Ok((lines, flattened))
    }

    fn emit_definition(&mut self, name: &str) -> Result<()> {
        self.emitted
            .entry(name.to_owned())
            .or_insert(self.client_sent);
        Ok(())
    }
}

fn quote_type(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        _ => "<non-path type>".to_owned(),
    }
}

/// Every struct and enum the listed sources declare, by name.
fn load_definitions(root: &Path) -> Result<BTreeMap<String, Definition>> {
    let mut definitions = BTreeMap::new();
    for source in SOURCES {
        let path = root.join(source);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {source}"))?;
        let file = syn::parse_file(&text).with_context(|| format!("parsing {source}"))?;
        for item in file.items {
            match item {
                syn::Item::Struct(item) => {
                    definitions.insert(item.ident.to_string(), Definition::Struct(item));
                }
                syn::Item::Enum(item) => {
                    definitions.insert(item.ident.to_string(), Definition::Enum(item));
                }
                _ => {}
            }
        }
    }
    Ok(definitions)
}

/// Render one struct as an interface (or a type alias when it flattens others).
fn render_struct(generator: &mut Generator, item: &syn::ItemStruct) -> Result<String> {
    let attrs = serde_attributes(&item.attrs)?;
    let name = Generator::ts_name(&item.ident.to_string());
    let mut out = String::new();
    write_docs(&mut out, &doc_lines(&item.attrs), "")?;
    match &item.fields {
        syn::Fields::Named(fields) => {
            let (lines, flattened) = generator.fields_text(fields, attrs.rename_all.as_deref())?;
            if flattened.is_empty() {
                writeln!(out, "export interface {name} {{")?;
                for field in &lines {
                    field.render(&mut out, "  ")?;
                }
                writeln!(out, "}}")?;
            } else {
                writeln!(out, "export type {name} = {} & {{", flattened.join(" & "))?;
                for field in &lines {
                    field.render(&mut out, "  ")?;
                }
                writeln!(out, "}};")?;
            }
        }
        syn::Fields::Unnamed(fields) if attrs.transparent || fields.unnamed.len() == 1 => {
            let inner = &fields.unnamed.first().context("newtype field")?.ty;
            let (text, optional) = generator.ts_type(inner)?;
            let text = if optional {
                format!("{text} | null")
            } else {
                text
            };
            writeln!(out, "export type {name} = {text};")?;
        }
        syn::Fields::Unnamed(_) => bail!("tuple struct `{name}` has no wire shape"),
        syn::Fields::Unit => writeln!(out, "export type {name} = null;")?,
    }
    Ok(out)
}

/// Render an enum: unit enums as string unions with a runtime list, tagged
/// enums as discriminated unions, adjacently tagged enums as an action map.
fn render_enum(
    generator: &mut Generator,
    item: &syn::ItemEnum,
    social_actions: &mut Vec<String>,
    social_requests: &mut Vec<(String, Vec<FieldText>)>,
) -> Result<String> {
    let attrs = serde_attributes(&item.attrs)?;
    if attrs.untagged {
        bail!(
            "`{}` is untagged: map it in STRING_TYPES or give it a tag",
            item.ident
        );
    }
    let name = Generator::ts_name(&item.ident.to_string());
    let client = name == "ClientMessage";
    // The reply sender stamps the request's correlation id on any server frame;
    // a client frame's id is read from the envelope (`RequestHeader`), so only
    // variants that declare `request_id` carry it in their type.
    let envelope = name == "ServerMessage";
    let rename_all = attrs.rename_all.as_deref();
    let mut out = String::new();
    write_docs(&mut out, &doc_lines(&item.attrs), "")?;
    let all_unit = item
        .variants
        .iter()
        .all(|variant| matches!(variant.fields, syn::Fields::Unit));
    if all_unit {
        let names: Vec<String> = item
            .variants
            .iter()
            .map(|variant| {
                let attrs = serde_attributes(&variant.attrs)?;
                Ok(attrs
                    .rename
                    .unwrap_or_else(|| rename(&variant.ident.to_string(), rename_all)))
            })
            .collect::<Result<_>>()?;
        let list = names
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let constant = screaming(&name);
        let line = format!("export const {constant} = [{list}] as const;");
        if line.len() <= 100 {
            writeln!(out, "{line}")?;
        } else {
            writeln!(out, "export const {constant} = [")?;
            for value in &names {
                writeln!(out, "  '{value}',")?;
            }
            writeln!(out, "] as const;")?;
        }
        writeln!(out, "export type {name} = (typeof {constant})[number];")?;
        return Ok(out);
    }
    match (attrs.tag.as_deref(), attrs.content.as_deref()) {
        (Some(tag), None) => {
            writeln!(out, "export type {name} =")?;
            for variant in &item.variants {
                let variant_attrs = serde_attributes(&variant.attrs)?;
                let discriminant = variant_attrs
                    .rename
                    .clone()
                    .unwrap_or_else(|| rename(&variant.ident.to_string(), rename_all));
                match &variant.fields {
                    syn::Fields::Unit => {
                        if client && social_actions.contains(&discriminant) {
                            social_requests.push((discriminant.clone(), Vec::new()));
                        }
                        if envelope {
                            writeln!(out, "  | {{ {tag}: '{discriminant}'; requestId?: string }}")?;
                        } else {
                            writeln!(out, "  | {{ {tag}: '{discriminant}' }}")?;
                        }
                    }
                    syn::Fields::Named(fields) => {
                        let (mut lines, flattened) = generator.fields_text(
                            fields,
                            variant_attrs
                                .rename_all
                                .as_deref()
                                .or(attrs.rename_all_fields.as_deref()),
                        )?;
                        let has_request_id = lines.iter().any(|field| field.named("requestId"));
                        if client && social_actions.contains(&discriminant) {
                            let payload = lines
                                .iter()
                                .filter(|field| !field.named("requestId"))
                                .cloned()
                                .collect::<Vec<_>>();
                            social_requests.push((discriminant.clone(), payload));
                        }
                        if envelope && !has_request_id {
                            lines.insert(0, FieldText::bare("requestId?: string;"));
                        }
                        if flattened.len() == 1 && generator.envelopes.contains_key(&flattened[0]) {
                            // The flattened action enum carries this variant's fields itself.
                            writeln!(out, "  | {}", flattened[0])?;
                            continue;
                        }
                        let mut body = format!("      {tag}: '{discriminant}';\n");
                        for field in &lines {
                            field.render(&mut body, "      ")?;
                        }
                        let object = format!("{{\n{body}    }}");
                        if flattened.is_empty() {
                            writeln!(out, "  | {object}")?;
                        } else {
                            writeln!(out, "  | ({object} & {})", flattened.join(" & "))?;
                        }
                    }
                    syn::Fields::Unnamed(fields) => {
                        let inner = &fields.unnamed.first().context("newtype variant")?.ty;
                        let (text, _) = generator.ts_type(inner)?;
                        writeln!(out, "  | ({{ {tag}: '{discriminant}' }} & {text})")?;
                    }
                }
            }
            // Close the union after the last variant.
            let trimmed = out.trim_end_matches('\n').to_owned();
            out = format!("{trimmed};\n");
        }
        (Some(tag), Some(content)) => {
            // An action map: the key is the tag value, the value the payload shape.
            let map_name = format!("{name}s");
            let base = name.strip_suffix("Response").unwrap_or(&name).to_owned();
            writeln!(out, "export interface {map_name} {{")?;
            for variant in &item.variants {
                let variant_attrs = serde_attributes(&variant.attrs)?;
                let key = variant_attrs
                    .rename
                    .clone()
                    .unwrap_or_else(|| rename(&variant.ident.to_string(), rename_all));
                social_actions.push(key.clone());
                match &variant.fields {
                    syn::Fields::Unit => writeln!(out, "  {key}: undefined;")?,
                    syn::Fields::Named(fields) => {
                        let (lines, flattened) = generator.fields_text(
                            fields,
                            variant_attrs
                                .rename_all
                                .as_deref()
                                .or(attrs.rename_all_fields.as_deref()),
                        )?;
                        let mut body = String::new();
                        for field in &lines {
                            field.render(&mut body, "    ")?;
                        }
                        let object = format!("{{\n{body}  }}");
                        if flattened.is_empty() {
                            writeln!(out, "  {key}: {object};")?;
                        } else {
                            writeln!(out, "  {key}: {object} & {};", flattened.join(" & "))?;
                        }
                    }
                    syn::Fields::Unnamed(fields) => {
                        let inner = &fields.unnamed.first().context("newtype variant")?.ty;
                        let (text, _) = generator.ts_type(inner)?;
                        writeln!(out, "  {key}: {text};")?;
                    }
                }
            }
            writeln!(out, "}}")?;
            writeln!(out, "export type {base}Action = keyof {map_name};")?;
            // The frame that carries this enum contributes its own fields (type, requestId).
            let envelope_fields = generator
                .envelopes
                .get(item.ident.to_string().as_str())
                .map(|fields| fields.join(" ") + " ")
                .unwrap_or_default();
            writeln!(
                out,
                "export type {name}<A extends {base}Action = {base}Action> = {{\n  [K in A]: {{ {envelope_fields}{tag}: K; {content}: {map_name}[K] }};\n}}[A];"
            )?;
        }
        (None, _) => bail!("enum `{name}` is externally tagged; the wire never uses that"),
    }
    Ok(out)
}

/// Action enums that a tagged frame flattens: `ServerMessage::SocialResponse { request_id,
/// #[serde(flatten)] response }` makes `SocialResponse` carry `type` and `requestId`.
fn envelopes(definitions: &BTreeMap<String, Definition>) -> Result<BTreeMap<String, Vec<String>>> {
    let mut found = BTreeMap::new();
    for definition in definitions.values() {
        let Definition::Enum(item) = definition else {
            continue;
        };
        let attrs = serde_attributes(&item.attrs)?;
        let Some(tag) = attrs.tag.clone() else {
            continue;
        };
        if attrs.content.is_some() {
            continue;
        }
        for variant in &item.variants {
            let syn::Fields::Named(fields) = &variant.fields else {
                continue;
            };
            let variant_attrs = serde_attributes(&variant.attrs)?;
            let mut target = None;
            for field in &fields.named {
                if serde_attributes(&field.attrs)?.flatten
                    && let syn::Type::Path(path) = &field.ty
                {
                    target = Some(Generator::last_segment(path)?.0);
                }
            }
            let Some(target) = target else {
                continue;
            };
            let Some(Definition::Enum(inner)) = definitions.get(&target) else {
                continue;
            };
            if serde_attributes(&inner.attrs)?.content.is_none() {
                continue;
            }
            let discriminant = variant_attrs
                .rename
                .clone()
                .unwrap_or_else(|| rename(&variant.ident.to_string(), attrs.rename_all.as_deref()));
            let mut envelope = vec![format!("{tag}: '{discriminant}';")];
            for field in &fields.named {
                let field_attrs = serde_attributes(&field.attrs)?;
                if field_attrs.flatten {
                    continue;
                }
                let ident = field.ident.as_ref().context("named field")?.to_string();
                let rule = variant_attrs
                    .rename_all
                    .as_deref()
                    .or(attrs.rename_all_fields.as_deref());
                let name = field_attrs
                    .rename
                    .clone()
                    .unwrap_or_else(|| rename(&ident, rule));
                match &field.ty {
                    syn::Type::Path(path)
                        if STRING_TYPES.contains(&Generator::last_segment(path)?.0.as_str()) =>
                    {
                        envelope.push(format!("{name}: string;"));
                    }
                    _ => bail!(
                        "envelope field `{ident}` of `{}` must be a string",
                        item.ident
                    ),
                }
            }
            found.insert(target, envelope);
        }
    }
    Ok(found)
}

/// The constant holding an enum's values: `ReportStatus` becomes `REPORT_STATUSES`.
fn screaming(name: &str) -> String {
    let singular = snake_case(name).to_uppercase();
    let suffix = if singular.ends_with('S') { "ES" } else { "S" };
    singular + suffix
}

/// The listed integer limits, each with its doc comment, ahead of the types.
fn render_constants(root: &Path) -> Result<String> {
    let mut files: BTreeMap<&str, syn::File> = BTreeMap::new();
    let mut out = String::new();
    for &(source, name) in CONSTANTS {
        if !files.contains_key(source) {
            let text = std::fs::read_to_string(root.join(source))
                .with_context(|| format!("reading {source}"))?;
            let file = syn::parse_file(&text).with_context(|| format!("parsing {source}"))?;
            files.insert(source, file);
        }
        let item = files[source]
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Const(item) if item.ident == name => Some(item),
                _ => None,
            })
            .with_context(|| format!("no constant `{name}` in {source}"))?;
        let value = match &*item.expr {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(value),
                ..
            }) => value.base10_digits().to_owned(),
            _ => bail!("constant `{name}` in {source} is not an integer literal"),
        };
        write_docs(&mut out, &doc_lines(&item.attrs), "")?;
        writeln!(out, "export const {name} = {value};")?;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

/// One rendered field: its doc lines and its `name: type;` text, indented by
/// whichever shape prints it.
#[derive(Clone)]
struct FieldText {
    docs: Vec<String>,
    text: String,
}

impl FieldText {
    fn bare(text: &str) -> Self {
        Self {
            docs: Vec::new(),
            text: text.to_owned(),
        }
    }

    fn named(&self, name: &str) -> bool {
        self.text.starts_with(name)
    }

    fn render(&self, out: &mut String, indent: &str) -> Result<()> {
        write_docs(out, &self.docs, indent)?;
        writeln!(out, "{indent}{}", self.text)?;
        Ok(())
    }
}

/// An item's `///` lines as JSDoc at the item's indent; Prettier leaves a
/// `*`-aligned block alone, so this is its final shape.
fn write_docs(out: &mut String, lines: &[String], indent: &str) -> Result<()> {
    match lines {
        [] => {}
        [line] => writeln!(out, "{indent}/** {line} */")?,
        lines => {
            writeln!(out, "{indent}/**")?;
            for line in lines {
                if line.is_empty() {
                    writeln!(out, "{indent} *")?;
                } else {
                    writeln!(out, "{indent} * {line}")?;
                }
            }
            writeln!(out, "{indent} */")?;
        }
    }
    Ok(())
}

/// The `///` lines of an item, trimmed.
fn doc_lines(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|attr| match &attr.meta {
            syn::Meta::NameValue(meta) if meta.path.is_ident("doc") => match &meta.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(text),
                    ..
                }) => Some(text.value().trim().to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn generate(root: &Path) -> Result<String> {
    let definitions = load_definitions(root)?;
    let mut generator = Generator {
        definitions,
        emitted: BTreeMap::new(),
        used_mediasoup: BTreeSet::new(),
        needs_media_kind: false,
        client_sent: false,
        envelopes: BTreeMap::new(),
    };
    generator.envelopes = envelopes(&generator.definitions)?;
    let roots = [
        ("ErrorCode", false),
        ("Role", false),
        ("SocialResponse", false),
        ("ClientMessage", true),
        ("ServerMessage", false),
        ("ChatPreferences", false),
        ("PreferencePatch", true),
        ("AuthResponse", false),
        ("AccountProfile", false),
        ("PublicProfile", false),
        ("RoomListItem", false),
        ("CreateRoomRequest", true),
    ];
    let mut body = String::new();
    let mut queue: Vec<(String, bool)> = roots
        .iter()
        .map(|(name, client)| ((*name).to_owned(), *client))
        .collect();
    let mut rendered: BTreeSet<String> = BTreeSet::new();
    let mut social_actions: Vec<String> = Vec::new();
    let mut social_requests: Vec<(String, Vec<FieldText>)> = Vec::new();
    while let Some((name, client)) = queue.first().cloned() {
        queue.remove(0);
        if rendered.contains(&name) {
            continue;
        }
        rendered.insert(name.clone());
        generator.client_sent = client;
        let definition = generator
            .definitions
            .get(&name)
            .with_context(|| format!("no wire definition for `{name}`"))?;
        let text = match definition {
            Definition::Struct(item) => {
                let item = item.clone();
                render_struct(&mut generator, &item)?
            }
            Definition::Enum(item) => {
                let item = item.clone();
                render_enum(
                    &mut generator,
                    &item,
                    &mut social_actions,
                    &mut social_requests,
                )?
            }
        };
        body.push_str(&text);
        body.push('\n');
        // Types discovered while rendering come next, in discovery order.
        let discovered: Vec<(String, bool)> = generator
            .emitted
            .iter()
            .filter(|(discovered, _)| {
                !rendered.contains(*discovered)
                    && !queue.iter().any(|(queued, _)| queued == *discovered)
            })
            .map(|(discovered, client)| (discovered.clone(), *client))
            .collect();
        queue.extend(discovered);
    }
    if !social_requests.is_empty() {
        writeln!(
            body,
            "/** The action selects both the outgoing payload and the validated response. */"
        )?;
        writeln!(body, "export interface SocialRequests {{")?;
        for (action, payload) in &social_requests {
            if payload.is_empty() {
                writeln!(body, "  {action}: undefined;")?;
            } else {
                writeln!(body, "  {action}: {{")?;
                for field in payload {
                    field.render(&mut body, "    ")?;
                }
                writeln!(body, "  }};")?;
            }
        }
        writeln!(body, "}}")?;
        body.push_str(SOCIAL_HELPERS);
    }
    let mut out = String::new();
    writeln!(
        out,
        "// Generated from the Rust wire types by `cargo run -q -p xtask -- protocol-ts`."
    )?;
    writeln!(
        out,
        "// Do not edit: change the Rust types and limits xtask/src/protocol_ts.rs lists and regenerate."
    )?;
    writeln!(out)?;
    if !generator.used_mediasoup.is_empty() {
        // The server finalizes router capabilities into the client capability shape.
        let names = generator
            .used_mediasoup
            .iter()
            .map(|name| {
                if name == "RtpCapabilitiesFinalized" {
                    "RtpCapabilities as RtpCapabilitiesFinalized".to_owned()
                } else {
                    name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let line = format!("import type {{ {names} }} from 'mediasoup-client/types';");
        if line.len() <= 100 {
            writeln!(out, "{line}")?;
        } else {
            writeln!(out, "import type {{")?;
            for name in names.split(", ") {
                writeln!(out, "  {name},")?;
            }
            writeln!(out, "}} from 'mediasoup-client/types';")?;
        }
        writeln!(out)?;
    }
    if generator.needs_media_kind {
        writeln!(
            out,
            "export const MEDIA_KINDS = ['audio', 'video'] as const;"
        )?;
        writeln!(out, "export type MediaKind = (typeof MEDIA_KINDS)[number];")?;
        writeln!(out)?;
    }
    out.push_str(&render_constants(root)?);
    out.push_str(&body);
    Ok(out)
}
