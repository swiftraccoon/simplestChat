//! A JSON document that keeps the order of every record's fields.
//!
//! Evidence is written with its fields in the order they were given, as the
//! reference wrote it, and a document that is read keeps the order it was
//! written in. A record that names a field twice is no document: it is
//! refused when the record ends, and the refusal says so.

use crate::record::quoted;
use serde::de::{DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor};
use serde_json::Number;
use std::{cell::Cell, fmt};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// They are no JSON, or more than one value.
    Invalid,
    /// A record names a field twice.
    Duplicate,
}

/// Reads one value and says whether a record named a field twice.
struct Reader<'a>(&'a Cell<bool>);

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
        let mut twice = false;
        while let Some(name) = fields.next_key::<String>()? {
            twice |= record.iter().any(|(held, _)| *held == name);
            record.push((name, fields.next_value_seed(Reader(self.0))?));
        }
        if twice {
            self.0.set(true);
            return Err(A::Error::custom("a field is named twice"));
        }
        Ok(Document::Record(record))
    }
}

impl Document {
    /// The one document that `content` is.
    pub fn read(content: &[u8]) -> Result<Self, Unread> {
        let twice = Cell::new(false);
        let mut source = serde_json::Deserializer::from_slice(content);
        let read = Reader(&twice).deserialize(&mut source);
        let read = read.and_then(|document| source.end().map(|()| document));
        read.map_err(|_| {
            if twice.get() {
                Unread::Duplicate
            } else {
                Unread::Invalid
            }
        })
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

    /// The document as the reference writes one to be read: two spaces a
    /// level, ASCII only, and a line end after it.
    pub fn pretty(&self) -> String {
        let mut text = String::new();
        self.spread(0, &mut text);
        text.push('\n');
        text
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
