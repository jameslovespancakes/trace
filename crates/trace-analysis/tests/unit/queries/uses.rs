use super::*;
use trace_core::Language;

fn row(kind: &'static str) -> ReferenceRow {
    ReferenceRow {
        kind,
        tier: "proven",
        file: "a.py".into(),
        language: Language::Python,
        line: 1,
        column: 1,
        start_byte: 0,
        end_byte: 1,
        text: String::new(),
        owner: None,
        via: None,
        source: "index".into(),
        resolution: "call_hierarchy",
        when: Vec::new(),
    }
}

#[test]
fn counts_follow_the_row_kinds() {
    let rows = [
        row("declaration"),
        row("call"),
        row("call"),
        row("override"),
        row("implements"),
        row("read"),
    ];
    let c = row_counts(&rows, 2);
    assert_eq!(
        c,
        UsesCounts {
            uses: 5,
            overrides: 2,
            declarations: 1,
            unresolved: 2,
            callers_of_callers: None,
            tests: None,
        }
    );
    assert_eq!(row_counts(&[], 0), UsesCounts::default());
}
