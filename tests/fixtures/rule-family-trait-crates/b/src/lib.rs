//! Crate `b`: printers implementing `a::Sink`.

use a::Sink;

pub mod testutil;

/// Records matching lines.
pub struct JsonSink {
    pub records: Vec<String>,
}

impl Sink for JsonSink {
    fn matched(&mut self, line: &str) -> bool {
        self.records.push(line.to_string());
        true
    }
}

/// Counts matches and context lines.
pub struct Summary {
    pub matches: usize,
    pub context: usize,
}

impl Sink for Summary {
    fn matched(&mut self, _line: &str) -> bool {
        self.matches += 1;
        true
    }

    fn context(&mut self, _line: &str) -> bool {
        self.context += 1;
        true
    }
}

/// Runs a search into a summary through a mutable reference.
pub fn summarize(lines: &[&str]) -> usize {
    let mut summary = Summary {
        matches: 0,
        context: 0,
    };
    a::search(&mut summary, lines);
    summary.matches
}
