use super::*;

fn spans(src: &str, names: &[&str]) -> Vec<ByteSpan> {
    names
        .iter()
        .map(|n| {
            let start = src.find(n).expect("decl");
            let end = src[start..].find("\n\n").map_or(src.len(), |e| start + e);
            ByteSpan::new(start as u32, end as u32)
        })
        .collect()
}

#[test]
fn renamed_functions_are_identical_and_different_ones_are_not() {
    let src = "def alpha(a, b):\n    \"\"\"Doc.\"\"\"\n    total = a + b\n    return helper(total, 1)\n\n\
                   def beta(x, y):\n    s = x + y\n    return other(s, 2)\n\n\
                   def gamma(items):\n    for i in items:\n        if i:\n            print(i)\n    return None\n";
    let fps =
        fingerprints(Language::Python, src.as_bytes(), &spans(src, &["def alpha", "def beta", "def gamma"]));
    let (a, b, c) = (fps[0].clone().unwrap(), fps[1].clone().unwrap(), fps[2].clone().unwrap());
    assert!((jaccard(&a, &b) - 1.0).abs() < 1e-9, "{}", jaccard(&a, &b));
    assert!(jaccard(&a, &c) < 0.6);
    assert_eq!(jaccard(&Fingerprint::default(), &Fingerprint::default()), 0.0);
}

#[test]
fn tiny_or_unknown_inputs_have_no_fingerprint() {
    assert_eq!(fingerprints(Language::Sql, b"select 1", &[ByteSpan::new(0, 8)]), vec![None]);
    assert_eq!(fingerprints(Language::Python, b"x = 1\n", &[ByteSpan::new(0, 0)]), vec![None]);
}
