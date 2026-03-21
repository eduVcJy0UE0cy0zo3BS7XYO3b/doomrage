use crate::db::Db;
use crate::render::RenderBlock;
use crate::scheme_engine::{SchemeEngine, ScriptResult};
use crate::types::{NodeId, Value};
use std::collections::HashMap;

/// Work request — stored, executed next frame in main thread
pub enum WorkRequest {
    Preview {
        node_id: NodeId,
        code: String,
        db: Db,
    },
    Compute {
        node_id: NodeId,
        code: String,
        available_inputs: HashMap<String, Value>,
        db: Db,
    },
}

pub enum WorkResult {
    Preview {
        node_id: NodeId,
        blocks: Vec<RenderBlock>,
    },
    Compute {
        node_id: NodeId,
        result: ScriptResult,
    },
    Error {
        node_id: NodeId,
        message: String,
    },
}

/// Deferred queue — queues requests, processes one per frame in main thread
pub struct DeferredQueue {
    queue: Vec<WorkRequest>,
}

impl DeferredQueue {
    pub fn new() -> Self {
        Self { queue: Vec::new() }
    }

    pub fn send(&mut self, request: WorkRequest) {
        self.queue.push(request);
    }

    pub fn cancel(&mut self) {
        self.queue.clear();
    }

    /// Process the next queued request. Call once per frame.
    pub fn poll(&mut self, engine: &SchemeEngine) -> Option<WorkResult> {
        let request = self.queue.pop()?;
        Some(execute_request(engine, request))
    }

    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }
}

fn execute_request(engine: &SchemeEngine, request: WorkRequest) -> WorkResult {
    match request {
        WorkRequest::Preview {
            node_id,
            code,
            db,
        } => {
            match engine.preview_script(Some(&db), &code) {
                Ok(r) => WorkResult::Preview {
                    node_id,
                    blocks: r.render_blocks,
                },
                Err(e) => WorkResult::Preview {
                    node_id,
                    blocks: vec![RenderBlock::Text(format!("Error: {}", e))],
                },
            }
        }
        WorkRequest::Compute {
            node_id,
            code,
            available_inputs,
            db,
        } => match engine.execute_script(&available_inputs, Some(&db), &code) {
            Ok(result) => WorkResult::Compute { node_id, result },
            Err(e) => WorkResult::Error {
                node_id,
                message: e.to_string(),
            },
        },
    }
}
