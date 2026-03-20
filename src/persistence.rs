use crate::types::Graph;
use anyhow::Result;
use std::path::Path;

pub fn save_graph(graph: &Graph, path: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(graph)?;
    std::fs::write(path, json)?;
    Ok(())
}

pub fn load_graph(path: &Path) -> Result<Graph> {
    let json = std::fs::read_to_string(path)?;
    let graph: Graph = serde_json::from_str(&json)?;
    Ok(graph)
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
