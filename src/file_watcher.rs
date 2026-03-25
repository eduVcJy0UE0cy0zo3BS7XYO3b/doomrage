//! Watch ~/.canvas/nodes/ for file changes and notify the runtime.
//! Any .scm file modification triggers: find node by path → update script_code → recompute.

use crate::persistence;
use crate::graph_runtime::GraphRuntime;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// Events produced by the file watcher.
pub enum FileEvent {
    /// A .scm node file was modified. (canvas_name, label, new_code)
    NodeChanged { canvas: String, label: String, code: String },
}

/// A running file watcher. Drop to stop watching.
pub struct FileWatcher {
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    rx: mpsc::Receiver<FileEvent>,
}

impl FileWatcher {
    /// Start watching ~/.canvas/nodes/ recursively.
    pub fn start() -> Result<Self, Box<dyn std::error::Error>> {
        Self::watch_dir(persistence::nodes_dir())
    }

    /// Start watching a specific directory recursively.
    pub fn watch_dir(nodes_dir: std::path::PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&nodes_dir)?;

        let (tx, rx) = mpsc::channel();

        let mut debouncer = new_debouncer(
            Duration::from_millis(200),
            move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
                let events = match events {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("File watcher error: {}", e);
                        return;
                    }
                };
                for event in events {
                    if event.kind != DebouncedEventKind::Any {
                        continue;
                    }
                    let path = &event.path;
                    if path.extension().map_or(true, |e| e != "scm") {
                        continue;
                    }
                    if let Some(file_event) = path_to_event(path) {
                        let _ = tx.send(file_event);
                    }
                }
            },
        )?;

        debouncer.watcher().watch(
            &nodes_dir,
            notify::RecursiveMode::Recursive,
        )?;

        log::info!("File watcher started on {}", nodes_dir.display());

        Ok(FileWatcher {
            _debouncer: debouncer,
            rx,
        })
    }

    /// Poll for file change events (non-blocking).
    pub fn poll(&self) -> Vec<FileEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }
}

/// Convert a file path to a FileEvent by extracting canvas/label from path structure.
/// Expected: ~/.canvas/nodes/{canvas}/{label}.scm
fn path_to_event(path: &std::path::Path) -> Option<FileEvent> {
    let label = path.file_stem()?.to_str()?.to_string();
    let canvas = path.parent()?.file_name()?.to_str()?.to_string();

    let code = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return None, // File deleted or unreadable
    };

    Some(FileEvent::NodeChanged { canvas, label, code })
}

/// Apply file events to a GraphRuntime: update node code and recompute.
pub fn apply_file_events(runtime: &mut GraphRuntime, events: Vec<FileEvent>) {
    for event in events {
        match event {
            FileEvent::NodeChanged { canvas, label, code } => {
                let graph = match runtime.all_graphs.get_mut(&canvas) {
                    Some(g) => g,
                    None => continue,
                };
                let node_id = graph.nodes.iter()
                    .find(|(_, n)| n.label.replace(' ', "-") == label && !n.phantom)
                    .map(|(&id, _)| id);
                if let Some(node_id) = node_id {
                    if let Some(node) = graph.nodes.get_mut(&node_id) {
                        // Only update if code actually changed
                        let new_hash = crate::types::content_hash(&code);
                        if node.code_hash != new_hash {
                            log::info!("File changed: {}/{} — recomputing", canvas, label);
                            node.set_code(code);
                            // Recompute this node
                            let inputs = runtime.all_graphs.get(&canvas)
                                .unwrap().resolve_all_input_values(node_id);
                            let template = runtime.registry.templates
                                .get(&runtime.all_graphs.get(&canvas).unwrap().nodes[&node_id].template_name)
                                .cloned();
                            runtime.pending_nodes.insert(node_id);
                            let node_ref = &runtime.all_graphs.get(&canvas).unwrap().nodes[&node_id];
                            let hr = crate::graph_runtime::resolve_hash_imports(node_ref, &runtime.all_graphs, &runtime.db);
                            runtime.actor_runtime.compute(
                                node_id,
                                node_ref.clone(),
                                template,
                                inputs,
                                hr,
                                runtime.db.clone(),
                            );
                        }
                    }
                }
            }
        }
    }
}
