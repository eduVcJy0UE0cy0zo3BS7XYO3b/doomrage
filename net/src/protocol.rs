use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    F64(f64),
    F32(f32),
    I64(i64),
    I32(i32),
    Bool(bool),
    Str(String),
}

impl Value {
    pub fn port_type_str(&self) -> &'static str {
        match self {
            Value::F64(_) => "f64",
            Value::F32(_) => "f32",
            Value::I64(_) => "i64",
            Value::I32(_) => "i32",
            Value::Bool(_) => "bool",
            Value::Str(_) => "string",
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            Value::F64(v) => *v,
            Value::F32(v) => *v as f64,
            Value::I64(v) => *v as f64,
            Value::I32(v) => *v as f64,
            Value::Bool(v) => if *v { 1.0 } else { 0.0 },
            Value::Str(_) => f64::NAN,
        }
    }

    pub fn to_scheme_literal(&self) -> String {
        match self {
            Value::F64(f) => format!("{}", f),
            Value::F32(f) => format!("{}", f),
            Value::I64(i) => format!("{}", i),
            Value::I32(i) => format!("{}", i),
            Value::Bool(b) => if *b { "#t" } else { "#f" }.to_string(),
            Value::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Value::F64(v) => format!("{v:.6}"),
            Value::F32(v) => format!("{v:.4}"),
            Value::I64(v) => format!("{v}"),
            Value::I32(v) => format!("{v}"),
            Value::Bool(v) => format!("{v}"),
            Value::Str(v) => v.clone(),
        }
    }
}

/// Gossipsub wire message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    pub channel: String,
    pub values: HashMap<String, Value>,
    pub seq: u64,
}

/// Client → net node (JSON Lines over TCP or stdin)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMsg {
    Publish {
        channel: String,
        values: HashMap<String, Value>,
    },
    ConnectRelay {
        addr: String,
    },
    DialPeer {
        addr: String,
    },
}

/// Net node → client (JSON Lines over TCP or stdout)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMsg {
    PeerId(String),
    PeerDiscovered(String),
    PeerLost(String),
    ValuesReceived {
        peer: String,
        channel: String,
        values: HashMap<String, Value>,
    },
}

// Keep old names as aliases for compatibility
pub type StdioIn = ClientMsg;
pub type StdioOut = ServerMsg;
