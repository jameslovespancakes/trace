use super::*;
use trace_core::facts::{CallSite, CallbackArg};
use trace_core::model::{ByteSpan, Provider};
use trace_core::semantics::{LibraryFile, SemLibraryCall};

fn empty_semantics() -> FileSemantics {
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: String::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    }
}

fn call(callee: &str, start: u32) -> CallSite {
    CallSite {
        owner: None,
        lexical_owner: None,
        span: ByteSpan::new(start, start + 20),
        callee_span: ByteSpan::new(start, start + callee.len() as u32),
        callee: callee.to_string(),
        member: Some(callee.to_string()),
        receiver: None,
        line: 1,
        activation: Default::default(),
        is_new: false,
        arg_count: 1,
    }
}

fn file(dir: &std::path::Path, name: &str, callee_start: u32) -> (FileFacts, FileSemantics) {
    let lib = dir.join(format!("{name}_lib.py"));
    std::fs::write(&lib, "def run(fn):\n    return fn()\n").expect("write");
    let mut facts = FileFacts::default();
    facts.calls.push(call("run", callee_start));
    facts.callbacks.push(CallbackArg {
        call_callee_span: ByteSpan::new(callee_start, callee_start + 3),
        callee: "run".to_string(),
        arg_span: ByteSpan::new(callee_start + 4, callee_start + 8),
        argument: "work".to_string(),
        name: "work".to_string(),
        owner: None,
        index: Some(0),
        keyword: None,
    });
    let mut sem = empty_semantics();
    sem.library_files.push(LibraryFile {
        path: lib.to_string_lossy().into_owned(),
        package: "pkg".to_string(),
        version: Some("1.0".to_string()),
        stdlib: false,
        readable: true,
        language: Language::Python,
    });
    sem.library_calls.push(SemLibraryCall {
        at: ByteSpan::new(callee_start, callee_start + 3),
        line: 1,
        file: 0,
        decl_line: 0,
        decl_column: 4,
        symbol: None,
    });
    (facts, sem)
}

fn view<'a>(path: &'a str, f: &'a (FileFacts, FileSemantics)) -> FileView<'a> {
    FileView {
        path,
        language: Language::Python,
        facts: Some(&f.0),
        semantics: Some(&f.1),
    }
}

fn normalized(mut k: LibraryKnowledge) -> LibraryKnowledge {
    k.stats.seconds = 0.0;
    k.stats.cache_hits = 0;
    k
}

#[test]
fn rule_incremental_knowledge_equals_full() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(&dir.path().join("cache")).expect("library");
    let a = file(dir.path(), "a", 10);
    let b = file(dir.path(), "b", 30);
    let full_before = knowledge(&[view("a.py", &a), view("b.py", &b)], &library);
    assert_eq!(full_before.by_call.len(), 2);
    assert_eq!(full_before.stats.sites, 2);
    // Edit b: its call moves; a is untouched.
    let b2 = file(dir.path(), "b", 50);
    let files = [view("a.py", &a), view("b.py", &b2)];
    let delta = IndexDelta {
        modified: ["b.py".to_string()].into_iter().collect(),
        requeried: ["b.py".to_string()].into_iter().collect(),
        ..IndexDelta::default()
    };
    let incremental = knowledge_delta(&files, &library, full_before.clone(), &delta);
    let full = knowledge(&files, &library);
    assert_eq!(normalized(incremental.clone()), normalized(full));
    assert!(incremental.by_call.contains_key(&("b.py".to_string(), 50)));
    assert!(!incremental.by_call.contains_key(&("b.py".to_string(), 30)));
    // Remove a.
    let files = [view("b.py", &b2)];
    let delta = IndexDelta {
        removed: ["a.py".to_string()].into_iter().collect(),
        ..IndexDelta::default()
    };
    let incremental = knowledge_delta(&files, &library, incremental, &delta);
    assert_eq!(normalized(incremental), normalized(knowledge(&files, &library)));
}

#[test]
fn rule_library_call_passing_a_function_is_requested() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = file(dir.path(), "a", 10);
    let reqs = requests(&[view("a.py", &a)]);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].args.len(), 1);
    assert_eq!(reqs[0].args[0].index, Some(0));
    assert!(reqs[0].target.is_some());
    // A call without a library location and without a blind unresolved record is not a
    // spelling candidate.
    let mut b = file(dir.path(), "b", 10);
    b.1.library_calls.clear();
    assert!(requests(&[view("b.py", &b)]).is_empty());
}
