use anyhow::Result;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Persistent key-value store backed by a JSON file.
#[derive(Clone)]
pub struct Store {
    data: Arc<RwLock<HashMap<String, JsonValue>>>,
    path: PathBuf,
}

impl Store {
    pub fn load(path: &Path) -> Result<Self> {
        let data = if path.exists() {
            let text = std::fs::read_to_string(path)?;
            serde_json::from_str(&text).unwrap_or_default()
        } else {
            HashMap::new()
        };

        Ok(Self {
            data: Arc::new(RwLock::new(data)),
            path: path.to_path_buf(),
        })
    }

    pub fn save(&self) -> Result<()> {
        let data = self.data.read().unwrap();
        let json = serde_json::to_string_pretty(&*data)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<JsonValue> {
        let data = self.data.read().unwrap();
        data.get(key).cloned()
    }

    pub fn set(&self, key: &str, value: JsonValue) {
        let mut data = self.data.write().unwrap();
        data.insert(key.to_string(), value);
    }

    pub fn delete(&self, key: &str) {
        let mut data = self.data.write().unwrap();
        data.remove(key);
    }

    pub fn keys(&self) -> Vec<String> {
        let data = self.data.read().unwrap();
        data.keys().cloned().collect()
    }

    pub fn append(&self, key: &str, value: JsonValue) {
        let mut data = self.data.write().unwrap();
        let entry = data
            .entry(key.to_string())
            .or_insert_with(|| JsonValue::Array(vec![]));
        if let JsonValue::Array(arr) = entry {
            arr.push(value);
        }
    }

    /// Convert a store value to a Scheme expression string
    pub fn value_to_scheme(val: &JsonValue) -> String {
        match val {
            JsonValue::Null => "\"\"".to_string(),
            JsonValue::Bool(b) => if *b { "#t" } else { "#f" }.to_string(),
            JsonValue::Number(n) => {
                if let Some(f) = n.as_f64() {
                    format!("{}", f)
                } else {
                    format!("{}", n)
                }
            }
            JsonValue::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            JsonValue::Array(arr) => {
                let items: Vec<String> = arr.iter().map(Self::value_to_scheme).collect();
                format!("(list {})", items.join(" "))
            }
            JsonValue::Object(obj) => {
                let pairs: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| format!("(list \"{}\" {})", k, Self::value_to_scheme(v)))
                    .collect();
                format!("(list {})", pairs.join(" "))
            }
        }
    }

    /// Convert a Scheme display string back to JSON value (best-effort)
    pub fn scheme_to_value(s: &str) -> JsonValue {
        let s = s.trim();
        if s == "#t" {
            JsonValue::Bool(true)
        } else if s == "#f" {
            JsonValue::Bool(false)
        } else if let Ok(n) = s.parse::<f64>() {
            serde_json::Number::from_f64(n)
                .map(JsonValue::Number)
                .unwrap_or(JsonValue::String(s.to_string()))
        } else if s.starts_with('"') && s.ends_with('"') {
            JsonValue::String(s[1..s.len() - 1].to_string())
        } else {
            JsonValue::String(s.to_string())
        }
    }
}
