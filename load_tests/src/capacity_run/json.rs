//! Ordered Serde records written with the retained Python report's JSON framing.
//!
//! JSON float handling is explicit: zero-price reports contain Infinity, and
//! Python's scientific exponents and ASCII escapes are part of the reference.

use serde::Serialize;
use serde::ser::{self, Impossible, SerializeMap, SerializeSeq, SerializeStruct};

enum Node {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

#[derive(Debug)]
struct Error(String);
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

struct Builder;
struct Sequence(Vec<Node>);
struct Object {
    entries: Vec<(String, Node)>,
    key: Option<String>,
}

macro_rules! integer {
    ($($method:ident($type:ty)),* $(,)?) => {$(
        fn $method(self, value: $type) -> Result<Node, Error> {
            Ok(Node::Number(value.to_string()))
        }
    )*};
}

impl ser::Serializer for Builder {
    type Ok = Node;
    type Error = Error;
    type SerializeSeq = Sequence;
    type SerializeTuple = Sequence;
    type SerializeTupleStruct = Sequence;
    type SerializeTupleVariant = Impossible<Node, Error>;
    type SerializeMap = Object;
    type SerializeStruct = Object;
    type SerializeStructVariant = Impossible<Node, Error>;

    integer!(
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128)
    );

    fn serialize_bool(self, value: bool) -> Result<Node, Error> {
        Ok(Node::Bool(value))
    }
    fn serialize_f32(self, value: f32) -> Result<Node, Error> {
        self.serialize_f64(value.into())
    }
    fn serialize_f64(self, value: f64) -> Result<Node, Error> {
        let text = if value.is_nan() {
            "NaN".into()
        } else if value == f64::INFINITY {
            "Infinity".into()
        } else if value == f64::NEG_INFINITY {
            "-Infinity".into()
        } else {
            crate::capacity::format::repr(value)
        };
        Ok(Node::Number(text))
    }
    fn serialize_char(self, value: char) -> Result<Node, Error> {
        self.serialize_str(&value.to_string())
    }
    fn serialize_str(self, value: &str) -> Result<Node, Error> {
        Ok(Node::String(value.into()))
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<Node, Error> {
        Ok(Node::Array(
            value
                .iter()
                .map(|byte| Node::Number(byte.to_string()))
                .collect(),
        ))
    }
    fn serialize_none(self) -> Result<Node, Error> {
        Ok(Node::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Node, Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Node, Error> {
        Ok(Node::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Node, Error> {
        Ok(Node::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
    ) -> Result<Node, Error> {
        self.serialize_str(name)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Node, Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        value: &T,
    ) -> Result<Node, Error> {
        Ok(Node::Object(vec![(name.into(), value.serialize(self)?)]))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Sequence, Error> {
        Ok(Sequence(Vec::new()))
    }
    fn serialize_tuple(self, _: usize) -> Result<Sequence, Error> {
        self.serialize_seq(None)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Sequence, Error> {
        self.serialize_seq(None)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(Error(
            "capacity report tuple variants are unsupported".into(),
        ))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Object, Error> {
        Ok(Object {
            entries: Vec::new(),
            key: None,
        })
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Object, Error> {
        self.serialize_map(None)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(Error(
            "capacity report struct variants are unsupported".into(),
        ))
    }
}

impl SerializeSeq for Sequence {
    type Ok = Node;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.0.push(value.serialize(Builder)?);
        Ok(())
    }
    fn end(self) -> Result<Node, Error> {
        Ok(Node::Array(self.0))
    }
}

macro_rules! sequence {
    ($trait:ident, $method:ident) => {
        impl ser::$trait for Sequence {
            type Ok = Node;
            type Error = Error;
            fn $method<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
                SerializeSeq::serialize_element(self, value)
            }
            fn end(self) -> Result<Node, Error> {
                SerializeSeq::end(self)
            }
        }
    };
}
sequence!(SerializeTuple, serialize_element);
sequence!(SerializeTupleStruct, serialize_field);

impl SerializeMap for Object {
    type Ok = Node;
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.key = Some(match value.serialize(Builder)? {
            Node::String(value) | Node::Number(value) => value,
            Node::Bool(value) => value.to_string(),
            Node::Null => "null".into(),
            _ => return Err(Error("capacity report object key must be scalar".into())),
        });
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let key = self
            .key
            .take()
            .ok_or_else(|| Error("missing capacity report key".into()))?;
        self.entries.push((key, value.serialize(Builder)?));
        Ok(())
    }
    fn end(self) -> Result<Node, Error> {
        Ok(Node::Object(self.entries))
    }
}

impl SerializeStruct for Object {
    type Ok = Node;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.entries.push((name.into(), value.serialize(Builder)?));
        Ok(())
    }
    fn end(self) -> Result<Node, Error> {
        SerializeMap::end(self)
    }
}

fn quoted(value: &str) -> String {
    let source = serde_json::to_string(value).expect("string serialization cannot fail");
    let mut output = String::new();
    for character in source.chars() {
        if !character.is_ascii() || character == '\u{007f}' {
            for unit in character.encode_utf16(&mut [0; 2]) {
                output.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn write(node: Node, output: &mut String, depth: usize) {
    let newline = |output: &mut String, depth| {
        output.push('\n');
        for _ in 0..depth * 2 {
            output.push(' ');
        }
    };
    match node {
        Node::Null => output.push_str("null"),
        Node::Bool(value) => output.push_str(if value { "true" } else { "false" }),
        Node::Number(value) => output.push_str(&value),
        Node::String(value) => output.push_str(&quoted(&value)),
        Node::Array(values) => {
            output.push('[');
            let nonempty = !values.is_empty();
            for (index, value) in values.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                newline(output, depth + 1);
                write(value, output, depth + 1);
            }
            if nonempty {
                newline(output, depth);
            }
            output.push(']');
        }
        Node::Object(values) => {
            output.push('{');
            let nonempty = !values.is_empty();
            for (index, (key, value)) in values.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                newline(output, depth + 1);
                output.push_str(&quoted(&key));
                output.push_str(": ");
                write(value, output, depth + 1);
            }
            if nonempty {
                newline(output, depth);
            }
            output.push('}');
        }
    }
}

pub(super) fn bytes(value: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    let mut output = String::new();
    write(value.serialize(Builder)?, &mut output, 0);
    Ok(output.into_bytes())
}
