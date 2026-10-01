use std::io;

use crate::util::Replacer;

#[derive(Default)]
pub(crate) struct JSONSink<W: io::Write + Default> {
    replacer: Replacer,
    wtr: W,
}

impl<W: io::Write + Default> JSONSink<W> {
    /// The third `Replacer::clear` call, in a module behind `#[cfg(feature = "serde")]`.
    pub(crate) fn replace(&mut self, bytes: &[u8]) {
        self.replacer.clear();
        self.replacer.replace_all(bytes);
        let _ = self.wtr.flush();
    }
}

impl Default for JSONSink<Vec<u8>> {
    fn default() -> Self {
        JSONSink { replacer: Replacer::new(), wtr: Vec::new() }
    }
}
