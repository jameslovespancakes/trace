/// A type that provides replacements (ripgrep printer util.rs).
#[derive(Debug, Default)]
pub struct Replacer {
    space: Option<Vec<u8>>,
}

impl Replacer {
    pub fn new() -> Replacer {
        Replacer { space: None }
    }

    /// Clear space used for performing a replacement.
    pub fn clear(&mut self) {
        if let Some(ref mut space) = self.space {
            space.clear();
        }
    }

    pub fn replace_all(&mut self, bytes: &[u8]) {
        self.space = Some(bytes.to_vec());
    }
}
