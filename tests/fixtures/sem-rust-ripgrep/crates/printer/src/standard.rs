use crate::util::Replacer;

pub trait Matcher {
    fn is_match(&self, bytes: &[u8]) -> bool;
}

pub struct StandardSink<'p, M: Matcher> {
    matcher: &'p M,
    replacer: Replacer,
}

impl<'p, M: Matcher> StandardSink<'p, M> {
    /// Two calls to `Replacer::clear` in generic impl methods (ripgrep standard.rs).
    pub fn matched(&mut self, bytes: &[u8]) -> bool {
        self.replacer.clear();
        if self.matcher.is_match(bytes) {
            self.replacer.replace_all(bytes);
        }
        true
    }

    pub fn context(&mut self, bytes: &[u8]) -> bool {
        self.replacer.clear();
        !bytes.is_empty()
    }
}
