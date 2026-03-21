use anyhow::Result;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use surrealdb::engine::local::{Db as SurrealDb, Mem};
use surrealdb::Surreal;
use tokio::runtime::Runtime;

#[derive(Clone)]
pub struct Db {
    inner: Arc<DbInner>,
}

struct DbInner {
    rt: Runtime,
    surreal: Surreal<SurrealDb>,
}

impl Db {
    pub fn new() -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let surreal = rt.block_on(async {
            let db = Surreal::new::<Mem>(()).await?;
            db.use_ns("wasm-canvas").use_db("main").await?;
            Ok::<_, anyhow::Error>(db)
        })?;

        log::info!("SurrealDB (kv-mem) ready");
        Ok(Self {
            inner: Arc::new(DbInner { rt, surreal }),
        })
    }

    /// Run a SurrealQL query, return results as JSON array
    pub fn query(&self, surql: &str) -> Result<Vec<JsonValue>> {
        self.inner.rt.block_on(async {
            let mut response = self.inner.surreal.query(surql).await
                .map_err(|e| anyhow::anyhow!("DB query error: {}", e))?;

            // Count statements in the query (separated by ;)
            let num_statements = surql.chars().filter(|&c| c == ';').count() + 1;

            let mut all_results = Vec::new();
            for idx in 0..num_statements {
                match response.take::<Vec<JsonValue>>(idx) {
                    Ok(rows) => all_results.extend(rows),
                    Err(_) => break,
                }
            }
            Ok(all_results)
        })
    }

    /// Run a mutation (CREATE/UPDATE/DELETE), ignore results
    pub fn run(&self, surql: &str) -> Result<()> {
        self.inner.rt.block_on(async {
            self.inner.surreal.query(surql).await
                .map_err(|e| anyhow::anyhow!("DB run error: {}", e))?;
            Ok(())
        })
    }

    /// Export all data as JSON (for save)
    pub fn export(&self) -> Result<String> {
        let tables = self.query("INFO FOR DB")?;
        let mut dump = serde_json::Map::new();

        // Get table names from INFO FOR DB
        if let Some(info) = tables.first() {
            if let Some(tables_obj) = info.get("tables").and_then(|t| t.as_object()) {
                for table_name in tables_obj.keys() {
                    let rows = self.query(&format!("SELECT * FROM {}", table_name))?;
                    dump.insert(table_name.clone(), JsonValue::Array(rows));
                }
            }
        }

        Ok(serde_json::to_string_pretty(&dump)?)
    }

    /// Import data from JSON dump
    pub fn import(&self, json: &str) -> Result<()> {
        let data: serde_json::Map<String, JsonValue> = serde_json::from_str(json)?;
        for (table, rows) in &data {
            if let Some(arr) = rows.as_array() {
                // Clear table first
                self.run(&format!("DELETE {}", table))?;
                for row in arr {
                    let content = serde_json::to_string(row)?;
                    self.run(&format!("CREATE {} CONTENT {}", table, content))?;
                }
            }
        }
        Ok(())
    }

    // --- Key-value convenience ---

    /// Escape a string for safe use inside SurrealQL single quotes
    fn escape_surql(s: &str) -> String {
        s.replace('\\', "\\\\").replace('\'', "\\'")
    }

    /// Get a value from the `kv` table by key
    pub fn kv_get(&self, key: &str) -> Option<JsonValue> {
        let results = self
            .query(&format!(
                "SELECT * FROM kv WHERE key = '{}'",
                Self::escape_surql(key)
            ))
            .ok()?;
        results
            .first()
            .and_then(|row| row.get("value"))
            .cloned()
    }

    /// Set a value in the `kv` table
    pub fn kv_set(&self, key: &str, value: JsonValue) {
        let esc = Self::escape_surql(key);
        let val_str = serde_json::to_string(&value).unwrap_or_default();
        // DELETE + CREATE: avoids UPSERT record-id issues
        let _ = self.run(&format!("DELETE FROM kv WHERE key = '{}'", esc));
        let _ = self.run(&format!(
            "CREATE kv SET key = '{}', value = {}",
            esc, val_str
        ));
    }

    /// Delete a key from the `kv` table
    pub fn kv_delete(&self, key: &str) {
        let _ = self.run(&format!(
            "DELETE FROM kv WHERE key = '{}'",
            Self::escape_surql(key)
        ));
    }

    /// Append a value to a JSON array stored in kv
    pub fn kv_append(&self, key: &str, value: JsonValue) {
        let current = self.kv_get(key).unwrap_or(JsonValue::Array(vec![]));
        let mut arr = match current {
            JsonValue::Array(a) => a,
            _ => vec![current],
        };
        arr.push(value);
        self.kv_set(key, JsonValue::Array(arr));
    }

    /// Get all keys from the `kv` table
    pub fn kv_keys(&self) -> Vec<String> {
        self.query("SELECT * FROM kv")
            .unwrap_or_default()
            .iter()
            .filter_map(|row| row.get("key").and_then(|k| k.as_str()).map(|s| s.to_string()))
            .collect()
    }

    /// Get all key-value pairs in one query
    pub fn kv_all(&self) -> Vec<(String, JsonValue)> {
        self.query("SELECT * FROM kv")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| {
                let key = row.get("key")?.as_str()?.to_string();
                let value = row.get("value")?.clone();
                Some((key, value))
            })
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh_db() -> Db {
        Db::new().expect("Failed to create test DB")
    }

    // --- kv basic CRUD ---

    #[test]
    fn test_kv_set_get() {
        let db = fresh_db();
        db.kv_set("name", json!("Alice"));
        assert_eq!(db.kv_get("name"), Some(json!("Alice")));
    }

    #[test]
    fn test_kv_get_missing() {
        let db = fresh_db();
        assert_eq!(db.kv_get("nonexistent"), None);
    }

    #[test]
    fn test_kv_overwrite() {
        let db = fresh_db();
        db.kv_set("x", json!(1));
        db.kv_set("x", json!(2));
        assert_eq!(db.kv_get("x"), Some(json!(2)));
    }

    #[test]
    fn test_kv_delete() {
        let db = fresh_db();
        db.kv_set("tmp", json!("here"));
        assert!(db.kv_get("tmp").is_some());
        db.kv_delete("tmp");
        assert_eq!(db.kv_get("tmp"), None);
    }

    #[test]
    fn test_kv_delete_missing() {
        let db = fresh_db();
        db.kv_delete("ghost"); // should not panic
    }

    // --- kv value types ---

    #[test]
    fn test_kv_types() {
        let db = fresh_db();

        db.kv_set("bool", json!(true));
        assert_eq!(db.kv_get("bool"), Some(json!(true)));

        db.kv_set("num", json!(3.14));
        assert_eq!(db.kv_get("num"), Some(json!(3.14)));

        db.kv_set("null", json!(null));
        assert_eq!(db.kv_get("null"), Some(json!(null)));

        db.kv_set("arr", json!([1, 2, 3]));
        assert_eq!(db.kv_get("arr"), Some(json!([1, 2, 3])));

        db.kv_set("obj", json!({"a": 1}));
        assert_eq!(db.kv_get("obj"), Some(json!({"a": 1})));
    }

    // --- kv_append ---

    #[test]
    fn test_kv_append_new_key() {
        let db = fresh_db();
        db.kv_append("list", json!("first"));
        assert_eq!(db.kv_get("list"), Some(json!(["first"])));
    }

    #[test]
    fn test_kv_append_existing() {
        let db = fresh_db();
        db.kv_set("list", json!(["a"]));
        db.kv_append("list", json!("b"));
        assert_eq!(db.kv_get("list"), Some(json!(["a", "b"])));
    }

    // --- kv_keys / kv_all ---

    #[test]
    fn test_kv_keys() {
        let db = fresh_db();
        db.kv_set("alpha", json!(1));
        db.kv_set("beta", json!(2));
        let mut keys = db.kv_keys();
        keys.sort();
        assert_eq!(keys, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_kv_all() {
        let db = fresh_db();
        db.kv_set("x", json!(10));
        db.kv_set("y", json!(20));
        let all = db.kv_all();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|(k, v)| k == "x" && *v == json!(10)));
        assert!(all.iter().any(|(k, v)| k == "y" && *v == json!(20)));
    }

    // --- kv injection safety ---

    #[test]
    fn test_kv_injection_key() {
        let db = fresh_db();
        let evil_key = "'; DELETE kv; --";
        db.kv_set(evil_key, json!("safe"));
        // The key should be stored literally, not interpreted as SQL
        assert_eq!(db.kv_get(evil_key), Some(json!("safe")));
        // Other keys should be unaffected
        db.kv_set("innocent", json!("ok"));
        assert_eq!(db.kv_get("innocent"), Some(json!("ok")));
    }

    #[test]
    fn test_kv_special_chars_in_key() {
        let db = fresh_db();
        for key in &["spaces in key", "quote\"key", "back\\slash", "emoji🎉", ""] {
            db.kv_set(key, json!("val"));
            assert_eq!(db.kv_get(key), Some(json!("val")), "Failed for key: {:?}", key);
            db.kv_delete(key);
            assert_eq!(db.kv_get(key), None);
        }
    }

    // --- raw query / run ---

    #[test]
    fn test_query_create_select() {
        let db = fresh_db();
        db.run("CREATE tasks SET name = 'buy milk', done = false").unwrap();
        db.run("CREATE tasks SET name = 'walk dog', done = true").unwrap();

        let all = db.query("SELECT * FROM tasks").unwrap();
        assert_eq!(all.len(), 2);

        let done = db.query("SELECT * FROM tasks WHERE done = true").unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].get("name").and_then(|n| n.as_str()), Some("walk dog"));
    }

    #[test]
    fn test_query_update_delete() {
        let db = fresh_db();
        db.run("CREATE tasks:1 SET name = 'test', done = false").unwrap();

        db.run("UPDATE tasks:1 SET done = true").unwrap();
        let rows = db.query("SELECT * FROM tasks:1").unwrap();
        assert_eq!(rows[0].get("done"), Some(&json!(true)));

        db.run("DELETE tasks:1").unwrap();
        let rows = db.query("SELECT * FROM tasks").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn test_query_error() {
        let db = fresh_db();
        // Invalid SurrealQL should return error, not panic
        assert!(db.run("NOT VALID SQL !!! %%%").is_err());
    }

    // --- export / import ---

    #[test]
    fn test_export_import_roundtrip() {
        let db1 = fresh_db();
        db1.kv_set("color", json!("blue"));
        db1.kv_set("count", json!(42));
        db1.run("CREATE tasks SET name = 'test task', done = false").unwrap();

        let dump = db1.export().unwrap();

        // Import into a fresh DB
        let db2 = fresh_db();
        assert!(db2.kv_get("color").is_none()); // empty before import
        db2.import(&dump).unwrap();

        assert_eq!(db2.kv_get("color"), Some(json!("blue")));
        assert_eq!(db2.kv_get("count"), Some(json!(42)));

        let tasks = db2.query("SELECT * FROM tasks").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].get("name").and_then(|n| n.as_str()), Some("test task"));
    }

    #[test]
    fn test_import_clears_existing() {
        let db = fresh_db();
        db.kv_set("old", json!("data"));

        // Import with only "new" key
        db.import(r#"{"kv": [{"id": "kv:new", "key": "new", "value": "fresh"}]}"#).unwrap();

        // Old data in kv table should be gone (DELETE kv runs first)
        assert_eq!(db.kv_get("old"), None);
        assert_eq!(db.kv_get("new"), Some(json!("fresh")));
    }

    #[test]
    fn test_import_empty() {
        let db = fresh_db();
        db.import("{}").unwrap(); // should not panic
    }

    // --- clone shares state ---

    #[test]
    fn test_clone_shares_data() {
        let db1 = fresh_db();
        let db2 = db1.clone();

        db1.kv_set("shared", json!("yes"));
        assert_eq!(db2.kv_get("shared"), Some(json!("yes")));
    }
}
