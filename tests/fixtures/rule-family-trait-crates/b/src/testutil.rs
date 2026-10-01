//! Test helpers: a sink that records everything.

use a::Sink;

/// Records every line.
pub struct KitchenSink {
    pub seen: Vec<String>,
}

impl Sink for KitchenSink {
    fn matched(&mut self, line: &str) -> bool {
        self.seen.push(line.to_string());
        true
    }
}
