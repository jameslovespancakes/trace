use super::*;

fn at(src: &str, needle: &str) -> u32 {
    src.find(needle).expect("needle") as u32
}

#[test]
fn python_result_uses() {
    let src = "def g(user):\n    token = f(user)\n    if token:\n        pass\n    print(token.value)\n    return token\n\ndef h():\n    f(1)\n\ndef k():\n    return await f(2)\n";
    let r = result_use(Language::Python, src.as_bytes(), at(src, "f(user)")).unwrap();
    assert_eq!(r.call, "token = f(user)");
    assert_eq!(r.line, 2);
    assert_eq!(r.stored_in, vec!["token".to_string()]);
    let kinds: Vec<&UseKind> = r.uses.iter().map(|u| &u.kind).collect();
    assert!(kinds.contains(&&UseKind::TestsTruthiness));
    assert!(kinds.contains(&&UseKind::ReadsAttribute));
    assert!(kinds.contains(&&UseKind::ReturnsIt));
    let attr = r.uses.iter().find(|u| u.kind == UseKind::ReadsAttribute).unwrap();
    assert_eq!(attr.line, 5);
    assert_eq!(attr.code, "print(token.value)");

    let r = result_use(Language::Python, src.as_bytes(), at(src, "f(1)")).unwrap();
    assert_eq!(r.uses[0].kind, UseKind::IgnoresResult);
    let r = result_use(Language::Python, src.as_bytes(), at(src, "f(2)")).unwrap();
    assert_eq!(r.uses[0].kind, UseKind::ReturnsIt);
    assert!(result_use(Language::Python, src.as_bytes(), 0).is_none());
}

#[test]
fn stored_without_uses_and_javascript() {
    let src = "def g():\n    data = load()\n";
    let r = result_use(Language::Python, src.as_bytes(), at(src, "load")).unwrap();
    assert_eq!(r.uses[0].kind, UseKind::StoredIn);
    assert_eq!(r.uses[0].code, "data");

    let js = "function g() {\n  const v = compute();\n  if (v > 1) { use(v); }\n  return `${v}`;\n}\n";
    let r = result_use(Language::JavaScript, js.as_bytes(), at(js, "compute")).unwrap();
    let kinds: Vec<&UseKind> = r.uses.iter().map(|u| &u.kind).collect();
    assert!(kinds.contains(&&UseKind::ComparesIt), "{kinds:?}");
    assert!(kinds.contains(&&UseKind::PassesToCall), "{kinds:?}");
    assert!(kinds.contains(&&UseKind::FormatsIntoText), "{kinds:?}");
}
