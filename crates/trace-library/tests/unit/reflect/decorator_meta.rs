use super::*;

fn write(dir: &Path, rel: &str, text: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dirs");
    std::fs::write(&p, text).expect("write");
    p
}

#[test]
fn rule_decorator_factory_metadata_writes_follow_closures_constants_and_defaults() {
    let dir = tempfile::tempdir().expect("temp");
    write(dir.path(), "lib/keys.js", "exports.ROUTE_KEY = exports.VERB_KEY = void 0;\nexports.ROUTE_KEY = 'route';\nexports.VERB_KEY = 'verb';\n");
    let lib = write(
        dir.path(),
        "lib/marks.js",
        "const keys_1 = require(\"./keys\");\n\
         const Mark = (options = {}) => {\n\
           const route = options[keys_1.ROUTE_KEY] ? options[keys_1.ROUTE_KEY] : '/';\n\
           const verb = options[keys_1.VERB_KEY] || Kinds.READ;\n\
           return (target, key, descriptor) => {\n\
             Store.put(keys_1.ROUTE_KEY, route, descriptor.value);\n\
             Store.put(keys_1.VERB_KEY, verb, descriptor.value);\n\
             return descriptor;\n\
           };\n\
         };\n\
         exports.Mark = Mark;\n\
         const make = (verb) => (route) => (0, exports.Mark)({ [keys_1.ROUTE_KEY]: route, [keys_1.VERB_KEY]: verb });\n\
         exports.Read = make(Kinds.READ);\n\
         function Group(prefixOrOptions) {\n\
           const [prefix, other] = isText(prefixOrOptions) ? [prefixOrOptions, undefined] : [prefixOrOptions.prefix || '/', prefixOrOptions.other];\n\
           return (target) => { Store.put(keys_1.ROUTE_KEY, prefix, target); };\n\
         }\n\
         exports.Group = Group;\n",
    );
    let text = std::fs::read_to_string(&lib).expect("read");
    let line_of = |needle: &str| text.lines().position(|l| l.contains(needle)).expect("line") as u32;
    let read = decorator_writes(&lib, line_of("exports.Read = make"), 0, 1, "Store.put", 0);
    assert!(
        read.iter().any(|w| w.key == "route"
            && w.on_member
            && w.value == MetaValue::Choice(vec![MetaValue::Arg(0), MetaValue::Str("/".into())])),
        "{read:?}"
    );
    assert!(
        read.iter()
            .any(|w| w.key == "verb" && w.value == MetaValue::Member("READ".into())),
        "{read:?}"
    );
    let group = decorator_writes(&lib, line_of("function Group"), 0, 1, "Store.put", 0);
    assert_eq!(group.len(), 1, "{group:?}");
    assert_eq!(group[0].key, "route");
    assert!(!group[0].on_member);
    assert!(matches!(&group[0].value, MetaValue::Choice(c) if c.contains(&MetaValue::Arg(0))), "{group:?}");
    // A call that is not the metadata store records nothing.
    assert!(decorator_writes(&lib, line_of("function Group"), 0, 1, "Other.put", 0).is_empty());
}
