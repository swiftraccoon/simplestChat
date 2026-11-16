//! A JSON document that keeps the order of every record's fields.
//!
//! Evidence is written with its fields in the order they were given, as the
//! reference wrote it, and a document that is read keeps the order it was
//! written in. A record that names a field twice is no document: it is
//! refused when the record ends, and the refusal says so.

use crate::record::quoted;
use serde::de::{DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor};
use serde_json::Number;
use std::{cell::RefCell, fmt};

/// One value of a document.
#[derive(Clone, Debug, PartialEq)]
pub enum Document {
    /// Nothing.
    Null,
    /// A truth value.
    Bool(bool),
    /// A number, as it was written.
    Number(Number),
    /// Text.
    Text(String),
    /// A list.
    List(Vec<Self>),
    /// A record: its fields in their order.
    Record(Vec<(String, Self)>),
}

/// Why bytes are no document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    /// They are no JSON, or more than one value.
    Invalid,
    /// A record names this field twice.
    Duplicate(String),
}

/// Reads one value and says which field a record named twice.
struct Reader<'a>(&'a RefCell<Option<String>>);

impl<'de> DeserializeSeed<'de> for Reader<'_> {
    type Value = Document;

    fn deserialize<D: Deserializer<'de>>(self, source: D) -> Result<Document, D::Error> {
        source.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Reader<'_> {
    type Value = Document;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Document, E> {
        Ok(Document::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Document, E> {
        Ok(Document::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Document, E> {
        Ok(Document::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Document, E> {
        Ok(Document::Number(value.into()))
    }

    fn visit_f64<E: Error>(self, value: f64) -> Result<Document, E> {
        let number = Number::from_f64(value).ok_or_else(|| E::custom("no finite number"))?;
        Ok(Document::Number(number))
    }

    fn visit_str<E>(self, value: &str) -> Result<Document, E> {
        Ok(Document::Text(value.to_owned()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Document, A::Error> {
        let mut list = Vec::new();
        while let Some(item) = items.next_element_seed(Reader(self.0))? {
            list.push(item);
        }
        Ok(Document::List(list))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> Result<Document, A::Error> {
        let mut record: Vec<(String, Document)> = Vec::new();
        let mut twice = None;
        while let Some(name) = fields.next_key::<String>()? {
            if twice.is_none() && record.iter().any(|(held, _)| *held == name) {
                twice = Some(name.clone());
            }
            record.push((name, fields.next_value_seed(Reader(self.0))?));
        }
        if let Some(name) = twice {
            self.0.replace(Some(name));
            return Err(A::Error::custom("a field is named twice"));
        }
        Ok(Document::Record(record))
    }
}

impl Document {
    /// The one document that `content` is.
    pub fn read(content: &[u8]) -> Result<Self, Unread> {
        let twice = RefCell::new(None);
        let mut source = serde_json::Deserializer::from_slice(content);
        let read = Reader(&twice).deserialize(&mut source);
        let read = read.and_then(|document| source.end().map(|()| document));
        read.map_err(|_| twice.take().map_or(Unread::Invalid, Unread::Duplicate))
    }

    /// The field `name` of a record.
    pub fn get(&self, name: &str) -> Option<&Self> {
        self.fields()?
            .iter()
            .find_map(|(held, value)| (held == name).then_some(value))
    }

    /// The fields of a record.
    pub fn fields(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Record(fields) => Some(fields),
            _ => None,
        }
    }

    /// The names of a record's fields.
    pub fn names(&self) -> std::collections::BTreeSet<&str> {
        let fields = self.fields().unwrap_or_default();
        fields.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// Set the field `name` of a record to `value`: where it is, or last.
    pub fn put(&mut self, name: &str, value: Self) {
        if let Self::Record(fields) = self {
            match fields.iter_mut().find(|(field, _)| field == name) {
                Some((_, slot)) => *slot = value,
                None => fields.push((name.to_owned(), value)),
            }
        }
    }

    /// The items of a list.
    pub fn items(&self) -> Option<&[Self]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    /// The text that this is.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The whole number that this is: one written without a fraction or an
    /// exponent that a machine word holds.
    pub fn whole(&self) -> Option<i128> {
        match self {
            Self::Number(number) => number
                .as_i64()
                .map(i128::from)
                .or_else(|| number.as_u64().map(i128::from)),
            _ => None,
        }
    }

    /// A record of `fields` in their order.
    pub fn record<const N: usize>(fields: [(&str, Self); N]) -> Self {
        Self::Record(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }

    /// The document with the fields of every record in the order of their
    /// names.
    #[must_use]
    pub fn sorted(self) -> Self {
        match self {
            Self::List(items) => Self::List(items.into_iter().map(Self::sorted).collect()),
            Self::Record(fields) => {
                let sorted = fields
                    .into_iter()
                    .map(|(name, value)| (name, value.sorted()));
                let mut fields: Vec<(String, Self)> = sorted.collect();
                fields.sort_by(|left, right| left.0.cmp(&right.0));
                Self::Record(fields)
            }
            other => other,
        }
    }

    /// The document as the reference writes one to be read: two spaces a
    /// level, ASCII only, and a line end after it.
    pub fn pretty(&self) -> String {
        let mut text = String::new();
        self.spread(0, &mut text);
        text.push('\n');
        text
    }

    /// The document as the reference writes one to bind: nothing between
    /// items, ASCII only, and no line end.
    pub fn compact(&self) -> String {
        let mut text = String::new();
        self.press(&mut text, ",", ":");
        text
    }

    /// The document as the reference writes one by default: a comma and a
    /// space between items, a colon and a space after a name, ASCII only,
    /// and no line end.
    pub fn spaced(&self) -> String {
        let mut text = String::new();
        self.press(&mut text, ", ", ": ");
        text
    }

    /// Write the document with `between` between items and `after` after a
    /// name.
    fn press(&self, text: &mut String, between: &str, after: &str) {
        match self {
            Self::List(items) => {
                text.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        text.push_str(between);
                    }
                    item.press(text, between, after);
                }
                text.push(']');
            }
            Self::Record(fields) => {
                text.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        text.push_str(between);
                    }
                    text.push_str(&quoted(name));
                    text.push_str(after);
                    value.press(text, between, after);
                }
                text.push('}');
            }
            scalar => scalar.spread(0, text),
        }
    }

    fn spread(&self, depth: usize, text: &mut String) {
        let open = |text: &mut String, index: usize| {
            text.push_str(if index == 0 { "\n" } else { ",\n" });
            text.push_str(&"  ".repeat(depth + 1));
        };
        let close = |text: &mut String, bracket: char| {
            text.push('\n');
            text.push_str(&"  ".repeat(depth));
            text.push(bracket);
        };
        match self {
            Self::Null => text.push_str("null"),
            Self::Bool(value) => text.push_str(if *value { "true" } else { "false" }),
            Self::Number(number) => text.push_str(&number.to_string()),
            Self::Text(string) => text.push_str(&quoted(string)),
            Self::List(items) if items.is_empty() => text.push_str("[]"),
            Self::Record(fields) if fields.is_empty() => text.push_str("{}"),
            Self::List(items) => {
                text.push('[');
                for (index, item) in items.iter().enumerate() {
                    open(text, index);
                    item.spread(depth + 1, text);
                }
                close(text, ']');
            }
            Self::Record(fields) => {
                text.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    open(text, index);
                    text.push_str(&quoted(name));
                    text.push_str(": ");
                    value.spread(depth + 1, text);
                }
                close(text, '}');
            }
        }
    }
}

impl From<bool> for Document {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<&str> for Document {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for Document {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<usize> for Document {
    fn from(value: usize) -> Self {
        Self::Number((value as u64).into())
    }
}

impl From<u64> for Document {
    fn from(value: u64) -> Self {
        Self::Number(value.into())
    }
}

impl From<Vec<Self>> for Document {
    fn from(value: Vec<Self>) -> Self {
        Self::List(value)
    }
}

impl<T: Into<Self>> From<Option<T>> for Document {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}
