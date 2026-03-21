use crate::db::Db;
use crate::render::RenderBlock;
use crate::scheme_engine::{parse_port_declarations, SchemeEngine, ScriptResult};
use crate::types::{NodeId, Value};

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
        input_bindings: Vec<(String, Value)>,
        output_names: Vec<String>,
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
            let (input_decls, _) = parse_port_declarations(&code);
            let input_names: Vec<String> = input_decls.iter().map(|d| d.name.clone()).collect();

            match engine.preview_script(&input_names, Some(&db), &code) {
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
            input_bindings,
            output_names,
            db,
        } => match engine.execute_script(&input_bindings, &output_names, Some(&db), &code) {
            Ok(result) => WorkResult::Compute { node_id, result },
            Err(e) => WorkResult::Error {
                node_id,
                message: e.to_string(),
            },
        },
    }
}
