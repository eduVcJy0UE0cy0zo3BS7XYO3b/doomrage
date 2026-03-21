use std::collections::VecDeque;
use std::time::Instant;

const MAX_ENTRIES: usize = 200;

#[derive(Clone, Debug)]
pub struct DebugEntry {
    pub elapsed_ms: u64,
    pub source: &'static str,
    pub message: String,
}

pub struct DebugLog {
    entries: VecDeque<DebugEntry>,
    start: Instant,
}

impl DebugLog {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            start: Instant::now(),
        }
    }

    pub fn log(&mut self, source: &'static str, message: impl Into<String>) {
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        self.entries.push_back(DebugEntry {
            elapsed_ms,
            source,
            message: message.into(),
        });
        if self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    pub fn entries(&self) -> &VecDeque<DebugEntry> {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
