use std::path::{Path, PathBuf};

pub struct WalkBuilder {
    paths: Vec<PathBuf>,
    cwd: Option<PathBuf>,
    flags: Vec<bool>,
    depth: Option<usize>,
}

impl WalkBuilder {
    pub fn new<P: AsRef<Path>>(path: P) -> WalkBuilder {
        WalkBuilder { paths: vec![path.as_ref().to_path_buf()], cwd: None, flags: Vec::new(), depth: None }
    }

    pub fn add<P: AsRef<Path>>(&mut self, path: P) -> &mut WalkBuilder {
        self.paths.push(path.as_ref().to_path_buf());
        self
    }

    pub fn max_depth(&mut self, depth: Option<usize>) -> &mut WalkBuilder {
        self.depth = depth;
        self
    }

    pub fn follow_links(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn max_filesize(&mut self, filesize: Option<u64>) -> &mut WalkBuilder {
        self.flag(filesize.is_some())
    }

    pub fn threads(&mut self, n: usize) -> &mut WalkBuilder {
        self.flag(n > 1)
    }

    pub fn same_file_system(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn skip_stdout(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn overrides(&mut self, overrides: u32) -> &mut WalkBuilder {
        self.flag(overrides > 0)
    }

    pub fn types(&mut self, types: u32) -> &mut WalkBuilder {
        self.flag(types > 0)
    }

    pub fn hidden(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn parents(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn ignore(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn git_global(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn git_ignore(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn git_exclude(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn require_git(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn ignore_case_insensitive(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn sort_by_file_name(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn standard_filters(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    pub fn filter_entry(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flag(yes)
    }

    /// Set the current working directory used for matching global gitignores.
    pub fn current_dir(&mut self, cwd: impl Into<PathBuf>) -> &mut WalkBuilder {
        self.cwd = Some(cwd.into());
        self
    }

    fn flag(&mut self, yes: bool) -> &mut WalkBuilder {
        self.flags.push(yes);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::WalkBuilder;

    #[test]
    fn builds() {
        let mut builder = WalkBuilder::new(".");
        builder.hidden(false);
    }
}
