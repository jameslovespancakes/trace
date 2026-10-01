use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct HiArgs {
    paths: Vec<PathBuf>,
    cwd: PathBuf,
    max_depth: Option<usize>,
    follow: bool,
    max_filesize: Option<u64>,
    threads: usize,
    one_file_system: bool,
    hidden: bool,
    no_ignore_parent: bool,
    no_ignore_dot: bool,
    no_ignore_vcs: bool,
    no_ignore_global: bool,
    no_ignore_exclude: bool,
    no_require_git: bool,
    ignore_file_case_insensitive: bool,
}

impl HiArgs {
    /// A 20-step builder chain ending in `.current_dir(&self.cwd)` (ripgrep hiargs.rs).
    pub(crate) fn walk_builder(&self) -> ignore::WalkBuilder {
        let mut builder = ignore::WalkBuilder::new(&self.paths[0]);
        for path in self.paths.iter().skip(1) {
            builder.add(path);
        }
        builder
            .max_depth(self.max_depth)
            .follow_links(self.follow)
            .max_filesize(self.max_filesize)
            .threads(self.threads)
            .same_file_system(self.one_file_system)
            .skip_stdout(true)
            .overrides(0)
            .types(0)
            .hidden(!self.hidden)
            .parents(!self.no_ignore_parent)
            .ignore(!self.no_ignore_dot)
            .git_global(!self.no_ignore_vcs && !self.no_ignore_global)
            .git_ignore(!self.no_ignore_vcs)
            .git_exclude(!self.no_ignore_vcs && !self.no_ignore_exclude)
            .require_git(!self.no_require_git)
            .ignore_case_insensitive(self.ignore_file_case_insensitive)
            .sort_by_file_name(true)
            .standard_filters(true)
            .filter_entry(true)
            .current_dir(&self.cwd);
        builder
    }
}
