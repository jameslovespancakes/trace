use super::*;

/// the catalogue texts of the core errors.
#[test]
fn rule_error_texts_follow_the_catalogue() {
    assert_eq!(
        CoreError::SymbolNotFound("parse_config".into()).to_string(),
        "No symbol named \"parse_config\"."
    );
    assert_eq!(
        CoreError::NoNamedSymbolAt {
            reference: "www/index.js:185".into(),
            nearest: vec!["drawCells (line 128)".into(), "getIndex (line 124)".into()],
        }
        .to_string(),
        "Line 185 of www/index.js is not inside a named function.\n       Nearest: drawCells (line 128), getIndex (line 124)"
    );
    assert_eq!(
        CoreError::Locked(PathBuf::from("x")).to_string(),
        "Another trace process is updating this index. Try again in a moment."
    );
    assert_eq!(
        CoreError::AmbiguousSymbol {
            reference: "save".into(),
            candidates: vec!["a.py:save".into(), "b.py:save".into(), "c.py:save".into()],
        }
        .to_string(),
        "\"save\" matches 3 symbols. Use one of: a.py:save, b.py:save, c.py:save"
    );
    assert!(CoreError::InvalidRoot(PathBuf::from("proj"))
        .to_string()
        .starts_with("Folder not found: "));
    assert_eq!(CoreError::InvalidRoot(PathBuf::new()).kind(), "invalid_root");
    assert_eq!(
        CoreError::Excluded(PathBuf::from("x")).to_string(),
        "This folder is excluded in your trace settings."
    );
    assert_eq!(CoreError::Excluded(PathBuf::new()).kind(), "invalid_root");
    // Every text starts with a capital letter (or trace's own name).
    let samples = [
        CoreError::io("a", io::Error::other("x")),
        CoreError::OutsideRoot(PathBuf::from("a")),
        CoreError::InvalidRelativePath("a".into()),
        CoreError::Sensitive("a".into()),
        CoreError::Symlink("a".into()),
        CoreError::InsideInspectedRoot(PathBuf::from("a")),
        CoreError::Limit("a".into()),
        CoreError::SourceChanged("a".into()),
        CoreError::UnknownFile("a".into()),
        CoreError::CacheCorrupt("a".into()),
        CoreError::CacheVersion {
            found: 1,
            expected: 2,
        },
        CoreError::CacheRoot("a".into()),
        CoreError::InvalidBounds("a".into()),
        CoreError::InvalidPosition("a".into()),
        CoreError::Config("a".into()),
        CoreError::Serialize("a".into()),
    ];
    for e in samples {
        let text = e.to_string();
        assert!(text.starts_with("trace ") || text.chars().next().is_some_and(char::is_uppercase), "{text}");
    }
}
