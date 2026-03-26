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
        let _start = std::time::Instant::now();
        let result = self.inner.rt.block_on(async {
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
        });
        crate::metrics::DB_QUERIES.inc();
        crate::metrics::DB_QUERY_DURATION.observe(_start.elapsed().as_secs_f64());
        if result.is_err() { crate::metrics::DB_ERRORS.inc(); }
        result
    }

    /// Run a mutation (CREATE/UPDATE/DELETE), ignore results
    pub fn run(&self, surql: &str) -> Result<()> {
        let _start = std::time::Instant::now();
        let result = self.inner.rt.block_on(async {
            self.inner.surreal.query(surql).await
                .map_err(|e| anyhow::anyhow!("DB run error: {}", e))?;
            Ok(())
        });
        crate::metrics::DB_QUERIES.inc();
        crate::metrics::DB_QUERY_DURATION.observe(_start.elapsed().as_secs_f64());
        if result.is_err() { crate::metrics::DB_ERRORS.inc(); }
        result
    }

    /// Export all data as JSON (for save)
    pub fn export(&self) -> Result<String> {
        let tables = self.query("INFO FOR DB")?;
        let mut dump = serde_json::Map::new();

        // Get table names from INFO FOR DB
        if let Some(info) = tables.first() {
            if let Some(tables_obj) = info.get("tables").and_then(|t| t.as_object()) {
                for table_name in tables_obj.keys() {
                    if !Self::is_safe_table_name(table_name) { continue; }
                    let rows = self.query(&format!("SELECT * FROM {}", table_name))?;
                    dump.insert(table_name.clone(), JsonValue::Array(rows));
                }
            }
        }

        Ok(serde_json::to_string_pretty(&dump)?)
    }

    /// Validate that a table name contains only safe characters (alphanumeric, underscore, hyphen).
    fn is_safe_table_name(name: &str) -> bool {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    }

    /// Import data from JSON dump
    pub fn import(&self, json: &str) -> Result<()> {
        let data: serde_json::Map<String, JsonValue> = serde_json::from_str(json)?;
        for (table, rows) in &data {
            if !Self::is_safe_table_name(table) {
                log::warn!("Skipping invalid table name in import: {:?}", table);
                continue;
            }
            if let Some(arr) = rows.as_array() {
                self.run(&format!("DELETE {}", table))?;
                for row in arr {
                    let content = serde_json::to_string(row)?;
                    // Only accept JSON objects for CONTENT (reject arrays/primitives)
                    if !content.starts_with('{') {
                        log::warn!("Skipping non-object row in import for table '{}'", table);
                        continue;
                    }
                    self.run(&format!("CREATE {} CONTENT {}", table, content))?;
                }
            }
        }
        Ok(())
    }

    // --- Key-value convenience ---

    /// Escape a string for safe use inside SurrealQL single quotes
    pub fn escape_surql(s: &str) -> String {
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

    // --- Definition name registry (Unison-style name DB) ---

    /// Register a definition in the name database.
    /// Old entries for the same canvas+node+name are marked superseded (not deleted).
    pub fn register_def(&self, hash: &str, name: &str, canvas: &str, node_label: &str, form: &str) {
        let esc = |s: &str| Self::escape_surql(s);
        // Mark previous entries as superseded (immutable history)
        let _ = self.run(&format!(
            "UPDATE def_names SET superseded = true WHERE canvas = '{}' AND node_label = '{}' AND name = '{}' AND superseded = false",
            esc(canvas), esc(node_label), esc(name)
        ));
        // Check if this exact hash+name+canvas+node already exists (avoid duplicates)
        let existing = self.query(&format!(
            "SELECT * FROM def_names WHERE hash = '{}' AND name = '{}' AND canvas = '{}' AND node_label = '{}' AND superseded = false",
            esc(hash), esc(name), esc(canvas), esc(node_label)
        )).unwrap_or_default();
        if existing.is_empty() {
            let _ = self.run(&format!(
                "CREATE def_names SET hash = '{}', name = '{}', canvas = '{}', node_label = '{}', form = '{}', superseded = false",
                esc(hash), esc(name), esc(canvas), esc(node_label), esc(form)
            ));
        }
    }

    /// Look up all current (non-superseded) names/locations for a given content hash.
    pub fn lookup_by_hash(&self, hash: &str) -> Vec<NameEntry> {
        self.query(&format!(
            "SELECT * FROM def_names WHERE hash = '{}' AND superseded = false",
            Self::escape_surql(hash)
        ))
        .unwrap_or_default()
        .into_iter()
        .filter_map(NameEntry::from_json)
        .collect()
    }

    /// Look up the first name/location for a given content hash.
    pub fn lookup_by_hash_first(&self, hash: &str) -> Option<NameEntry> {
        self.lookup_by_hash(hash).into_iter().next()
    }

    /// Look up current definition by name within a canvas.
    pub fn lookup_by_name(&self, name: &str, canvas: &str) -> Option<NameEntry> {
        self.query(&format!(
            "SELECT * FROM def_names WHERE name = '{}' AND canvas = '{}' AND superseded = false",
            Self::escape_surql(name),
            Self::escape_surql(canvas)
        ))
        .ok()?
        .into_iter()
        .find_map(NameEntry::from_json)
    }

    /// Get all current definitions for a canvas.
    pub fn all_definitions(&self, canvas: &str) -> Vec<NameEntry> {
        self.query(&format!(
            "SELECT * FROM def_names WHERE canvas = '{}' AND superseded = false",
            Self::escape_surql(canvas)
        ))
        .unwrap_or_default()
        .into_iter()
        .filter_map(NameEntry::from_json)
        .collect()
    }

    /// Mark all definitions for a node as superseded (before re-registering after code change).
    pub fn clear_node_defs(&self, canvas: &str, node_label: &str) {
        let _ = self.run(&format!(
            "UPDATE def_names SET superseded = true WHERE canvas = '{}' AND node_label = '{}' AND superseded = false",
            Self::escape_surql(canvas),
            Self::escape_surql(node_label)
        ));
    }

    /// Rename a definition in the Name DB: update the name for a given hash.
    pub fn rename_def(&self, hash: &str, old_name: &str, new_name: &str, canvas: &str) {
        let esc = |s: &str| Self::escape_surql(s);
        let _ = self.run(&format!(
            "UPDATE def_names SET name = '{}' WHERE hash = '{}' AND name = '{}' AND canvas = '{}' AND superseded = false",
            esc(new_name), esc(hash), esc(old_name), esc(canvas)
        ));
    }

    /// Get full history of a definition by name (all versions, including superseded).
    /// Returns entries ordered with current first, then superseded.
    pub fn def_history(&self, name: &str, canvas: &str) -> Vec<NameEntry> {
        self.query(&format!(
            "SELECT * FROM def_names WHERE name = '{}' AND canvas = '{}' ORDER BY superseded ASC",
            Self::escape_surql(name),
            Self::escape_surql(canvas)
        ))
        .unwrap_or_default()
        .into_iter()
        .filter_map(NameEntry::from_json)
        .collect()
    }
}

/// Entry in the definition name database.
#[derive(Debug, Clone)]
pub struct NameEntry {
    pub hash: String,
    pub name: String,
    pub canvas: String,
    pub node_label: String,
    pub form: String,
}

impl NameEntry {
    fn from_json(v: JsonValue) -> Option<Self> {
        Some(NameEntry {
            hash: v.get("hash")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            canvas: v.get("canvas")?.as_str()?.to_string(),
            node_label: v.get("node_label")?.as_str()?.to_string(),
            form: v.get("form")?.as_str()?.to_string(),
        })
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

    // --- Name DB ---

    #[test]
    fn test_register_and_lookup_by_hash() {
        let db = fresh_db();
        db.register_def("abc123", "gain", "main", "controls", "Simple");
        let entries = db.lookup_by_hash("abc123");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "gain");
        assert_eq!(entries[0].canvas, "main");
        assert_eq!(entries[0].node_label, "controls");
    }

    #[test]
    fn test_lookup_by_name() {
        let db = fresh_db();
        db.register_def("def456", "freq", "main", "controls", "Simple");
        let entry = db.lookup_by_name("freq", "main").unwrap();
        assert_eq!(entry.hash, "def456");
        assert_eq!(entry.form, "Simple");
    }

    #[test]
    fn test_same_hash_different_names() {
        let db = fresh_db();
        db.register_def("same_hash", "a", "main", "node1", "Simple");
        db.register_def("same_hash", "b", "main", "node2", "Simple");
        let entries = db.lookup_by_hash("same_hash");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_all_definitions() {
        let db = fresh_db();
        db.register_def("h1", "x", "main", "n1", "Simple");
        db.register_def("h2", "y", "main", "n2", "Function");
        db.register_def("h3", "z", "other", "n3", "Simple");
        let defs = db.all_definitions("main");
        assert_eq!(defs.len(), 2);
    }

    #[test]
    fn test_clear_node_defs() {
        let db = fresh_db();
        db.register_def("h1", "a", "main", "controls", "Simple");
        db.register_def("h2", "b", "main", "controls", "Function");
        db.register_def("h3", "c", "main", "synth", "Simple");
        db.clear_node_defs("main", "controls");
        let defs = db.all_definitions("main");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "c");
    }

    #[test]
    fn test_register_overwrites_same_name() {
        let db = fresh_db();
        db.register_def("old_hash", "x", "main", "n1", "Simple");
        db.register_def("new_hash", "x", "main", "n1", "Simple");
        let entry = db.lookup_by_name("x", "main").unwrap();
        assert_eq!(entry.hash, "new_hash");
    }

    #[test]
    fn test_name_db_survives_export_import() {
        let db1 = fresh_db();
        db1.register_def("h1", "gain", "main", "controls", "Simple");
        let dump = db1.export().unwrap();

        let db2 = fresh_db();
        db2.import(&dump).unwrap();
        let entry = db2.lookup_by_name("gain", "main").unwrap();
        assert_eq!(entry.hash, "h1");
    }

    #[test]
    fn test_immutable_history() {
        let db = fresh_db();
        // Version 1
        db.register_def("hash_v1", "x", "main", "n1", "Simple");
        assert_eq!(db.lookup_by_name("x", "main").unwrap().hash, "hash_v1");

        // Version 2: old entry becomes superseded, not deleted
        db.register_def("hash_v2", "x", "main", "n1", "Simple");
        assert_eq!(db.lookup_by_name("x", "main").unwrap().hash, "hash_v2");

        // History shows both versions
        let history = db.def_history("x", "main");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].hash, "hash_v2"); // current first
        assert_eq!(history[1].hash, "hash_v1"); // superseded second

        // Version 3
        db.register_def("hash_v3", "x", "main", "n1", "Simple");
        let history = db.def_history("x", "main");
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].hash, "hash_v3");
    }

    #[test]
    fn test_clear_node_defs_preserves_history() {
        let db = fresh_db();
        db.register_def("h1", "a", "main", "controls", "Simple");
        db.register_def("h2", "b", "main", "controls", "Function");

        // clear marks as superseded, not deleted
        db.clear_node_defs("main", "controls");
        assert!(db.all_definitions("main").is_empty()); // no current defs

        // But history preserved
        let hist_a = db.def_history("a", "main");
        assert_eq!(hist_a.len(), 1);
        assert_eq!(hist_a[0].hash, "h1");
    }
}
