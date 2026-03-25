use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bytes(Vec<u8>),
    Int(i64),
    List(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

impl Value {
    pub fn string(s: impl Into<String>) -> Self {
        Value::Bytes(s.into().into_bytes())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&Vec<Value>> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_dict()?.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    pub fn dict(pairs: Vec<(&str, Value)>) -> Self {
        let mut map = BTreeMap::new();
        for (k, v) in pairs {
            map.insert(k.to_string(), v);
        }
        Value::Dict(map)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) => write!(f, "{}", s),
                Err(_) => write!(f, "<bytes:{}>", b.len()),
            },
            Value::Int(n) => write!(f, "{}", n),
            Value::List(l) => {
                write!(f, "[")?;
                for (i, v) in l.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Dict(d) => {
                write!(f, "{{")?;
                for (i, (k, v)) in d.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

// --- Encoding ---

pub fn encode(value: &Value, w: &mut impl Write) -> io::Result<()> {
    match value {
        Value::Bytes(b) => {
            write!(w, "{}:", b.len())?;
            w.write_all(b)?;
        }
        Value::Int(n) => {
            write!(w, "i{}e", n)?;
        }
        Value::List(items) => {
            w.write_all(b"l")?;
            for item in items {
                encode(item, w)?;
            }
            w.write_all(b"e")?;
        }
        Value::Dict(map) => {
            w.write_all(b"d")?;
            for (key, val) in map {
                let kb = key.as_bytes();
                write!(w, "{}:", kb.len())?;
                w.write_all(kb)?;
                encode(val, w)?;
            }
            w.write_all(b"e")?;
        }
    }
    Ok(())
}

pub fn encode_to_vec(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    encode(value, &mut buf).unwrap();
    buf
}

// --- Decoding ---

#[derive(Debug)]
pub enum DecodeError {
    UnexpectedEof,
    InvalidFormat(String),
    Io(io::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEof => write!(f, "unexpected end of input"),
            DecodeError::InvalidFormat(msg) => write!(f, "invalid bencode: {}", msg),
            DecodeError::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            DecodeError::UnexpectedEof
        } else {
            DecodeError::Io(e)
        }
    }
}

fn read_byte(r: &mut impl Read) -> Result<u8, DecodeError> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

pub fn decode(r: &mut impl Read) -> Result<Value, DecodeError> {
    let first = read_byte(r)?;
    match first {
        b'i' => {
            // Integer: i<number>e
            let mut digits = Vec::new();
            loop {
                let b = read_byte(r)?;
                if b == b'e' { break; }
                digits.push(b);
            }
            let s = String::from_utf8(digits)
                .map_err(|_| DecodeError::InvalidFormat("invalid integer bytes".into()))?;
            let n: i64 = s.parse()
                .map_err(|_| DecodeError::InvalidFormat(format!("invalid integer: {}", s)))?;
            Ok(Value::Int(n))
        }
        b'l' => {
            // List: l<items>e — items can be any bencode type
            let mut items = Vec::new();
            loop {
                let peek = read_byte(r)?;
                if peek == b'e' { break; }
                let item = decode_after_peek(peek, r)?;
                items.push(item);
            }
            Ok(Value::List(items))
        }
        b'd' => {
            // Dict: d<key><value>...e — keys are always strings
            let mut map = BTreeMap::new();
            loop {
                let peek = read_byte(r)?;
                if peek == b'e' { break; }
                let key = String::from_utf8(decode_string_with_first(peek, r)?)
                    .map_err(|_| DecodeError::InvalidFormat("dict key not utf8".into()))?;
                let val = decode(r)?;
                map.insert(key, val);
            }
            Ok(Value::Dict(map))
        }
        b'0'..=b'9' => {
            Ok(Value::Bytes(decode_string_with_first(first, r)?))
        }
        other => {
            Err(DecodeError::InvalidFormat(format!("unexpected byte: {}", other as char)))
        }
    }
}

/// Decode a bencode value given the first byte has already been read.
/// Handles all types: strings, ints, lists, dicts.
fn decode_after_peek(first: u8, r: &mut impl Read) -> Result<Value, DecodeError> {
    match first {
        b'i' => {
            let mut digits = Vec::new();
            loop {
                let b = read_byte(r)?;
                if b == b'e' { break; }
                digits.push(b);
            }
            let s = String::from_utf8(digits)
                .map_err(|_| DecodeError::InvalidFormat("invalid integer bytes".into()))?;
            let n: i64 = s.parse()
                .map_err(|_| DecodeError::InvalidFormat(format!("invalid integer: {}", s)))?;
            Ok(Value::Int(n))
        }
        b'l' => {
            let mut items = Vec::new();
            loop {
                let peek = read_byte(r)?;
                if peek == b'e' { break; }
                items.push(decode_after_peek(peek, r)?);
            }
            Ok(Value::List(items))
        }
        b'd' => {
            let mut map = BTreeMap::new();
            loop {
                let peek = read_byte(r)?;
                if peek == b'e' { break; }
                let key_val = decode_string_with_first(peek, r)?;
                let key = String::from_utf8(key_val)
                    .map_err(|_| DecodeError::InvalidFormat("dict key not utf8".into()))?;
                let val = decode(r)?;
                map.insert(key, val);
            }
            Ok(Value::Dict(map))
        }
        b'0'..=b'9' => {
            Ok(Value::Bytes(decode_string_with_first(first, r)?))
        }
        other => Err(DecodeError::InvalidFormat(format!("unexpected byte: {}", other as char))),
    }
}

/// Decode a bencode byte string given the first digit of the length.
fn decode_string_with_first(first: u8, r: &mut impl Read) -> Result<Vec<u8>, DecodeError> {
    // Byte string: <length>:<data>
    let mut len_digits = vec![first];
    loop {
        let b = read_byte(r)?;
        if b == b':' { break; }
        if !b.is_ascii_digit() {
            return Err(DecodeError::InvalidFormat(format!("expected digit or ':', got '{}'", b as char)));
        }
        len_digits.push(b);
    }
    let len_str = String::from_utf8(len_digits)
        .map_err(|_| DecodeError::InvalidFormat("invalid length".into()))?;
    let len: usize = len_str.parse()
        .map_err(|_| DecodeError::InvalidFormat(format!("invalid length: {}", len_str)))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn decode_bytes(data: &[u8]) -> Result<Value, DecodeError> {
    let mut cursor = io::Cursor::new(data);
    decode(&mut cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_string() {
        let v = Value::string("hello");
        assert_eq!(encode_to_vec(&v), b"5:hello");
    }

    #[test]
    fn encode_integer() {
        let v = Value::Int(42);
        assert_eq!(encode_to_vec(&v), b"i42e");
    }

    #[test]
    fn encode_negative_integer() {
        let v = Value::Int(-7);
        assert_eq!(encode_to_vec(&v), b"i-7e");
    }

    #[test]
    fn encode_dict() {
        let v = Value::dict(vec![("op", Value::string("eval"))]);
        assert_eq!(encode_to_vec(&v), b"d2:op4:evale");
    }

    #[test]
    fn encode_list() {
        let v = Value::List(vec![Value::string("done")]);
        assert_eq!(encode_to_vec(&v), b"l4:donee");
    }

    #[test]
    fn decode_string() {
        let v = decode_bytes(b"5:hello").unwrap();
        assert_eq!(v, Value::string("hello"));
    }

    #[test]
    fn decode_integer() {
        let v = decode_bytes(b"i42e").unwrap();
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn decode_dict() {
        let v = decode_bytes(b"d2:op4:evale").unwrap();
        assert_eq!(v.get_str("op"), Some("eval"));
    }

    #[test]
    fn decode_nested() {
        let v = decode_bytes(b"d6:statusl4:doneee").unwrap();
        let list = v.get("status").unwrap().as_list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].as_str(), Some("done"));
    }

    #[test]
    fn roundtrip() {
        let original = Value::dict(vec![
            ("code", Value::string("(+ 1 2)")),
            ("id", Value::string("msg-1")),
            ("op", Value::string("eval")),
            ("session", Value::string("sess-abc")),
        ]);
        let encoded = encode_to_vec(&original);
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn decode_incomplete() {
        assert!(decode_bytes(b"5:hel").is_err());
    }
}
