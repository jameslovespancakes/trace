//! Crate `a`: the trait and a forwarding impl for `&mut S`.

/// Receives matches.
pub trait Sink {
    /// A matching line.
    fn matched(&mut self, line: &str) -> bool;

    /// A context line.
    fn context(&mut self, line: &str) -> bool {
        let _ = line;
        true
    }
}

impl<S: Sink + ?Sized> Sink for &mut S {
    fn matched(&mut self, line: &str) -> bool {
        (**self).matched(line)
    }

    fn context(&mut self, line: &str) -> bool {
        (**self).context(line)
    }
}

/// Counts matching lines.
pub fn search<S: Sink>(mut sink: S, lines: &[&str]) -> usize {
    let mut n = 0;
    for line in lines {
        if sink.matched(line) {
            n += 1;
        }
    }
    n
}
