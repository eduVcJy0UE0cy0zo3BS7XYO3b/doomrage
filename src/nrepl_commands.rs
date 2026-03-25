//! Commands that nREPL evaluator sends to the main loop for graph mutations.
//! The evaluator runs in nREPL server threads; graph mutations happen on the main thread.

use crate::types::{NodeId, Value};
use std::sync::mpsc;

/// A command from nREPL to the main loop.
pub enum NreplCommand {
    CreateCanvas {
        name: String,
        reply: mpsc::Sender<Result<(), String>>,
    },
    ListCanvases {
        reply: mpsc::Sender<Vec<String>>,
    },
    CreateNode {
        canvas: String,
        label: String,
        code: String,
        exports: Vec<String>,
        imports: Vec<(String, String)>,
        reply: mpsc::Sender<Result<String, String>>,
    },
    DeleteNode {
        canvas: String,
        label: String,
        reply: mpsc::Sender<Result<(), String>>,
    },
    UpdateNode {
        canvas: String,
        label: String,
        code: Option<String>,
        exports: Option<Vec<String>>,
        imports: Option<Vec<(String, String)>>,
        reply: mpsc::Sender<Result<(), String>>,
    },
    NodeState {
        canvas: String,
        label: String,
        reply: mpsc::Sender<Option<nrepl::NodeState>>,
    },
    ComputeNode {
        canvas: String,
        label: String,
        reply: mpsc::Sender<Result<(), String>>,
    },
}

/// Sender half — cloned into SchemeEvaluator.
pub type CommandSender = mpsc::Sender<NreplCommand>;
/// Receiver half — polled by main loop.
pub type CommandReceiver = mpsc::Receiver<NreplCommand>;

pub fn channel() -> (CommandSender, CommandReceiver) {
    mpsc::channel()
}
