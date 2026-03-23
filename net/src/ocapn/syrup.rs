use crate::protocol::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum SyrupValue {
    Bool(bool),
    Integer(i64),
    Float32(f32),
    Float64(f64),
    Bytestring(Vec<u8>),
    String(String),
    Symbol(String),
    List(Vec<SyrupValue>),
    Dict(Vec<(SyrupValue, SyrupValue)>),
    Record {
        label: Box<SyrupValue>,
        fields: Vec<SyrupValue>,
    },
}

// --- Encoding ---

pub fn encode(value: &SyrupValue) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_into(value, &mut buf);
    buf
}

fn encode_into(value: &SyrupValue, buf: &mut Vec<u8>) {
    match value {
        SyrupValue::Bool(true) => buf.push(b't'),
        SyrupValue::Bool(false) => buf.push(b'f'),
        SyrupValue::Integer(n) => {
            if *n >= 0 {
                buf.extend_from_slice(format!("{}+", n).as_bytes());
            } else {
                buf.extend_from_slice(format!("{}-", -n).as_bytes());
            }
        }
        SyrupValue::Float32(f) => {
            buf.push(b'F');
            buf.extend_from_slice(&f.to_be_bytes());
        }
        SyrupValue::Float64(f) => {
            buf.push(b'D');
            buf.extend_from_slice(&f.to_be_bytes());
        }
        SyrupValue::Bytestring(data) => {
            buf.extend_from_slice(format!("{}:", data.len()).as_bytes());
            buf.extend_from_slice(data);
        }
        SyrupValue::String(s) => {
            let bytes = s.as_bytes();
            buf.extend_from_slice(format!("{}\"", bytes.len()).as_bytes());
            buf.extend_from_slice(bytes);
        }
        SyrupValue::Symbol(s) => {
            let bytes = s.as_bytes();
            buf.extend_from_slice(format!("{}'", bytes.len()).as_bytes());
            buf.extend_from_slice(bytes);
        }
        SyrupValue::List(items) => {
            buf.push(b'[');
            for item in items {
                encode_into(item, buf);
            }
            buf.push(b']');
        }
        SyrupValue::Dict(entries) => {
            buf.push(b'{');
            for (k, v) in entries {
                encode_into(k, buf);
                encode_into(v, buf);
            }
            buf.push(b'}');
        }
        SyrupValue::Record { label, fields } => {
            buf.push(b'<');
            encode_into(label, buf);
            for field in fields {
                encode_into(field, buf);
            }
            buf.push(b'>');
        }
    }
}

// --- Decoding ---

#[derive(Debug)]
pub enum DecodeError {
    UnexpectedEnd,
    InvalidByte(u8),
    Utf8Error,
    InvalidNumber,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnexpectedEnd => write!(f, "unexpected end of input"),
            DecodeError::InvalidByte(b) => write!(f, "invalid byte: 0x{:02x}", b),
            DecodeError::Utf8Error => write!(f, "invalid UTF-8"),
            DecodeError::InvalidNumber => write!(f, "invalid number"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub fn decode(data: &[u8]) -> Result<(SyrupValue, usize), DecodeError> {
    if data.is_empty() {
        return Err(DecodeError::UnexpectedEnd);
    }

    match data[0] {
        b't' => Ok((SyrupValue::Bool(true), 1)),
        b'f' => Ok((SyrupValue::Bool(false), 1)),
        b'F' => {
            if data.len() < 5 {
                return Err(DecodeError::UnexpectedEnd);
            }
            let bytes: [u8; 4] = data[1..5].try_into().unwrap();
            Ok((SyrupValue::Float32(f32::from_be_bytes(bytes)), 5))
        }
        b'D' => {
            if data.len() < 9 {
                return Err(DecodeError::UnexpectedEnd);
            }
            let bytes: [u8; 8] = data[1..9].try_into().unwrap();
            Ok((SyrupValue::Float64(f64::from_be_bytes(bytes)), 9))
        }
        b'[' => {
            let mut items = Vec::new();
            let mut pos = 1;
            loop {
                if pos >= data.len() {
                    return Err(DecodeError::UnexpectedEnd);
                }
                if data[pos] == b']' {
                    return Ok((SyrupValue::List(items), pos + 1));
                }
                let (val, consumed) = decode(&data[pos..])?;
                items.push(val);
                pos += consumed;
            }
        }
        b'{' => {
            let mut entries = Vec::new();
            let mut pos = 1;
            loop {
                if pos >= data.len() {
                    return Err(DecodeError::UnexpectedEnd);
                }
                if data[pos] == b'}' {
                    return Ok((SyrupValue::Dict(entries), pos + 1));
                }
                let (k, kc) = decode(&data[pos..])?;
                pos += kc;
                let (v, vc) = decode(&data[pos..])?;
                pos += vc;
                entries.push((k, v));
            }
        }
        b'<' => {
            let mut pos = 1;
            if pos >= data.len() {
                return Err(DecodeError::UnexpectedEnd);
            }
            let (label, lc) = decode(&data[pos..])?;
            pos += lc;
            let mut fields = Vec::new();
            loop {
                if pos >= data.len() {
                    return Err(DecodeError::UnexpectedEnd);
                }
                if data[pos] == b'>' {
                    return Ok((
                        SyrupValue::Record {
                            label: Box::new(label),
                            fields,
                        },
                        pos + 1,
                    ));
                }
                let (val, consumed) = decode(&data[pos..])?;
                fields.push(val);
                pos += consumed;
            }
        }
        b if b.is_ascii_digit() => decode_length_prefixed(data),
        _ => Err(DecodeError::InvalidByte(data[0])),
    }
}

fn decode_length_prefixed(data: &[u8]) -> Result<(SyrupValue, usize), DecodeError> {
    let mut pos = 0;
    while pos < data.len() && data[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos >= data.len() {
        return Err(DecodeError::UnexpectedEnd);
    }

    let len_str = std::str::from_utf8(&data[..pos]).map_err(|_| DecodeError::Utf8Error)?;
    let len: usize = len_str.parse().map_err(|_| DecodeError::InvalidNumber)?;
    let tag = data[pos];
    let start = pos + 1;

    match tag {
        b'+' => Ok((SyrupValue::Integer(len as i64), start)),
        b'-' => Ok((SyrupValue::Integer(-(len as i64)), start)),
        b':' | b'"' | b'\'' => {
            let end = start + len;
            if end > data.len() {
                return Err(DecodeError::UnexpectedEnd);
            }
            match tag {
                b':' => Ok((SyrupValue::Bytestring(data[start..end].to_vec()), end)),
                b'"' => {
                    let s = std::str::from_utf8(&data[start..end])
                        .map_err(|_| DecodeError::Utf8Error)?;
                    Ok((SyrupValue::String(s.to_string()), end))
                }
                b'\'' => {
                    let s = std::str::from_utf8(&data[start..end])
                        .map_err(|_| DecodeError::Utf8Error)?;
                    Ok((SyrupValue::Symbol(s.to_string()), end))
                }
                _ => unreachable!(),
            }
        }
        _ => Err(DecodeError::InvalidByte(tag)),
    }
}

// --- Conversion: Value <-> SyrupValue ---

impl From<&Value> for SyrupValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::F64(f) => SyrupValue::Float64(*f),
            Value::F32(f) => SyrupValue::Float32(*f),
            Value::I64(i) => SyrupValue::Integer(*i),
            Value::I32(i) => SyrupValue::Integer(*i as i64),
            Value::Bool(b) => SyrupValue::Bool(*b),
            Value::Str(s) => SyrupValue::String(s.clone()),
        }
    }
}

impl TryFrom<&SyrupValue> for Value {
    type Error = &'static str;

    fn try_from(v: &SyrupValue) -> Result<Self, Self::Error> {
        match v {
            SyrupValue::Float64(f) => Ok(Value::F64(*f)),
            SyrupValue::Float32(f) => Ok(Value::F32(*f)),
            SyrupValue::Integer(i) => Ok(Value::I64(*i)),
            SyrupValue::Bool(b) => Ok(Value::Bool(*b)),
            SyrupValue::String(s) => Ok(Value::Str(s.clone())),
            _ => Err("cannot convert complex SyrupValue to Value"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(val: &SyrupValue) -> SyrupValue {
        let bytes = encode(val);
        let (decoded, consumed) = decode(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        decoded
    }

    #[test]
    fn test_bool() {
        assert_eq!(roundtrip(&SyrupValue::Bool(true)), SyrupValue::Bool(true));
        assert_eq!(roundtrip(&SyrupValue::Bool(false)), SyrupValue::Bool(false));
    }

    #[test]
    fn test_integer() {
        assert_eq!(roundtrip(&SyrupValue::Integer(0)), SyrupValue::Integer(0));
        assert_eq!(roundtrip(&SyrupValue::Integer(42)), SyrupValue::Integer(42));
        assert_eq!(roundtrip(&SyrupValue::Integer(-7)), SyrupValue::Integer(-7));
    }

    #[test]
    fn test_float64() {
        assert_eq!(roundtrip(&SyrupValue::Float64(3.14)), SyrupValue::Float64(3.14));
    }

    #[test]
    fn test_string() {
        let val = SyrupValue::String("hello world".into());
        assert_eq!(roundtrip(&val), val);
    }

    #[test]
    fn test_record() {
        let val = SyrupValue::Record {
            label: Box::new(SyrupValue::Symbol("op:start-session".into())),
            fields: vec![SyrupValue::String("v1".into())],
        };
        assert_eq!(roundtrip(&val), val);
    }

    #[test]
    fn test_value_conversion() {
        let v = Value::F64(3.14);
        let sv = SyrupValue::from(&v);
        let back = Value::try_from(&sv).unwrap();
        assert!(matches!(back, Value::F64(f) if (f - 3.14).abs() < 1e-10));
    }
}
