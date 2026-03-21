use crate::db::Db;
use crate::types::Graph;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Derive the DB dump path from a graph file path: graph.json → graph.db.json
fn db_path_for(graph_path: &Path) -> PathBuf {
    let stem = graph_path.file_stem().unwrap_or_default().to_string_lossy();
    graph_path.with_file_name(format!("{}.db.json", stem))
}

pub fn save_graph(graph: &Graph, path: &Path, db: &Db) -> Result<()> {
    let json = serde_json::to_string_pretty(graph)?;
    std::fs::write(path, json)?;

    // Save DB alongside
    let db_json = db.export()?;
    std::fs::write(db_path_for(path), db_json)?;

    Ok(())
}

pub fn load_graph(path: &Path, db: &Db) -> Result<Graph> {
    let json = std::fs::read_to_string(path)?;
    let graph: Graph = serde_json::from_str(&json)?;

    // Load DB if exists alongside
    let db_path = db_path_for(path);
    if db_path.exists() {
        let db_json = std::fs::read_to_string(&db_path)?;
        db.import(&db_json)?;
        log::info!("Loaded DB from {}", db_path.display());
    }

    Ok(graph)
}

/// Save just the DB to a default location (for auto-save on exit)
pub fn save_db(db: &Db, path: &Path) -> Result<()> {
    let db_json = db.export()?;
    std::fs::write(path, db_json)?;
    Ok(())
}

/// Load just the DB from a default location (for auto-load on start)
pub fn load_db(db: &Db, path: &Path) -> Result<()> {
    if path.exists() {
        let db_json = std::fs::read_to_string(path)?;
        db.import(&db_json)?;
        log::info!("Restored DB from {}", path.display());
    }
    Ok(())
}

pub struct UndoHistory {
    states: Vec<String>,
    current: usize,
    max_size: usize,
}

impl UndoHistory {
    pub fn new(max_size: usize) -> Self {
        Self {
            states: Vec::new(),
            current: 0,
            max_size,
        }
    }

    pub fn push(&mut self, graph: &Graph) {
        let json = serde_json::to_string(graph).unwrap_or_default();

        // If we're not at the end, truncate future states
        if self.current < self.states.len() {
            self.states.truncate(self.current);
        }

        self.states.push(json);
        if self.states.len() > self.max_size {
            self.states.remove(0);
        }
        self.current = self.states.len();
    }

    pub fn undo(&mut self) -> Option<Graph> {
        if self.current > 1 {
            self.current -= 1;
            serde_json::from_str(&self.states[self.current - 1]).ok()
        } else {
            None
        }
    }

    pub fn redo(&mut self) -> Option<Graph> {
        if self.current < self.states.len() {
            self.current += 1;
            serde_json::from_str(&self.states[self.current - 1]).ok()
        } else {
            None
        }
    }

    pub fn can_undo(&self) -> bool {
        self.current > 1
    }

    pub fn can_redo(&self) -> bool {
        self.current < self.states.len()
    }
}
