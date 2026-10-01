use super::*;
use crate::model::Effect;

const SAMPLE: &str = r#"{
  "schema": 1,
  "generated_by": "export_labels.py",
  "counts": {"python": {"known": 3, "negatives": 1, "hand_checked": 0}},
  "languages": {
    "python": {
      "known": [
        {"id": "builtins.map#0", "api": "builtins.map", "module": "builtins", "name": "map",
         "method": false, "arg": {"Pos": 0}, "effect": "calls", "accept": ["calls", "stored_then_called"],
         "native": true, "why_native": "C builtin", "label": "true", "label_source": "rules"},
        {"id": "threading.Thread#0", "api": "threading.Thread", "module": "threading", "name": "Thread",
         "method": false, "arg": {"PosOrKw": [1, "target"]}, "effect": "calls",
         "accept": ["calls", "stored_then_called"], "native": false, "why_native": null, "label": "true"},
        {"id": "collections.deque#0", "api": "collections.deque", "module": "collections", "name": "deque",
         "method": false, "arg": {"Pos": 0}, "effect": "iterates", "accept": ["iterates", "advances"],
         "native": false, "label": "true"}
      ],
      "negatives": [
        {"id": "neg:queue.Queue.put:item", "api": "queue.Queue.put", "param": "item", "arg": {"Kw": "item"},
         "forbidden": ["calls", "stored_then_called"], "label": "false"}
      ],
      "hand_checked": []
    },
    "go": {
      "known": [],
      "negatives": [],
      "hand_checked": [
        {"id": "go:sort.Slice:less", "api": "sort.Slice", "library": "go@1.25", "file": "sort/slice.go",
         "arg": {"Pos": 1}, "effect": "calls", "accept": ["calls"], "label": "true",
         "label_source": "hand-checked", "why": "less is called by the sort"}
      ]
    }
  }
}"#;

fn derived(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn rule_gate_reads_the_lab_label_schema() {
    let file = LabelFile::parse(SAMPLE).expect("the lab schema parses");
    assert_eq!(file.schema, 1);
    let python = file.labels("python");
    assert_eq!(python.len(), 4);
    let (part, thread) = python[1];
    assert_eq!(part, Part::Known);
    assert_eq!(thread.arg, Some(ArgSel::PosOrKw(1, "target".to_string())));
    assert_eq!(thread.module.as_deref(), Some("threading"));
    assert!(thread.positive(part));
    let (part, negative) = python[3];
    assert_eq!(part, Part::Negative);
    assert!(!negative.positive(part));
    assert_eq!(negative.forbidden_effects(), vec!["calls", "stored_then_called"]);
    let go = file.labels("go");
    assert_eq!(go.len(), 1);
    assert_eq!(go[0].0, Part::HandChecked);
    assert_eq!(go[0].1.file.as_deref(), Some("sort/slice.go"));
    assert!(file.labels("haskell").is_empty());
    // Scoring: a running effect on a positive label is correct, on a negative wrong.
    assert_eq!(judge(thread, Part::Known, Some(&derived(&["stored_then_called"]))), Verdict::Correct);
    assert_eq!(judge(thread, Part::Known, Some(&derived(&[]))), Verdict::Miss);
    assert_eq!(judge(thread, Part::Known, None), Verdict::NotFound);
    assert_eq!(judge(negative, Part::Negative, Some(&derived(&["calls"]))), Verdict::Wrong);
    assert_eq!(judge(negative, Part::Negative, Some(&derived(&["returns"]))), Verdict::Clean);
    let (_, deque) = python[2];
    assert_eq!(judge(deque, Part::Known, Some(&derived(&["iterates"]))), Verdict::NotCounted);
    let mut score = Score::default();
    score.add(thread, Part::Known, Verdict::Correct, "");
    score.add(negative, Part::Negative, Verdict::Wrong, "");
    assert_eq!(score.facts(), 2);
    assert!((score.precision() - 0.5).abs() < 1e-9);
    assert!(!score.passed(0.9));
    // Selectors of the labels find the derived parameter by position or name.
    let summary = FunctionSummary {
        params: vec![
            (ArgSel::PosOrKw(1, "target".to_string()), vec![Effect::StoredThenCalled(ArgSel::Pos(1))]),
            (ArgSel::Rest(2), vec![Effect::Iterates(ArgSel::Rest(2))]),
        ],
        ..FunctionSummary::default()
    };
    assert_eq!(effects_at(&summary, thread.arg.as_ref(), None), derived(&["stored_then_called"]));
    assert_eq!(
        effects_at(&summary, Some(&ArgSel::Kw("target".to_string())), None),
        derived(&["stored_then_called"])
    );
    assert_eq!(effects_at(&summary, Some(&ArgSel::Pos(3)), None), derived(&["iterates"]));
    assert!(effects_at(&summary, Some(&ArgSel::Pos(0)), None).is_empty());
}

#[test]
fn rule_gate_counts_native_labels_as_table_not_derivation() {
    let file = LabelFile::parse(SAMPLE).expect("parse");
    let python = file.labels("python");
    let (part, map) = python[0];
    assert!(map.native);
    // Whatever is derived, a native label is neither a fact nor a miss.
    assert_eq!(judge(map, part, Some(&derived(&["calls"]))), Verdict::Native);
    assert_eq!(judge(map, part, None), Verdict::Native);
    let mut score = Score::default();
    score.add(map, part, Verdict::Native, "");
    assert_eq!((score.native, score.facts()), (1, 0));
    assert!(score.misses.is_empty() && score.not_found.is_empty());
    assert!(!score.passed(0.9), "no derived fact: not passed");
}
