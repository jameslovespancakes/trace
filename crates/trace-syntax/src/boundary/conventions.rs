//! The `syntax_conventions` rows of the embedded library tables this module reads (the
//! irreducible package-specific names, each row saying why it cannot be derived).

use std::sync::OnceLock;

use trace_core::model::BridgeKind;
use trace_core::Language;

/// One `syntax_conventions` row of `assets/library/<lang>.json` (the rows are validated by
/// trace-library's table loader and its `rule_tables` tests; every row says why it cannot be
/// derived). The rule names are the generic structures this module reads; the rows hold
/// every package-specific name (binding attributes, generated RPC names, GraphQL resolver
/// conventions, addon loaders).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Convention {
    pub(super) rule: String,
    pub(super) bridge: Option<BridgeKind>,
    pub(super) symbol: Option<String>,
    pub(super) pattern: Option<String>,
    pub(super) value: Option<String>,
    /// `key: {"pos": i}`: the argument naming the export (registration calls).
    pub(super) key_pos: Option<usize>,
    /// `key: {"kw": "k"}`: the element naming a value (annotation elements).
    pub(super) key_kw: Option<String>,
    /// `handler: {"pos": i}`: the argument holding the exported function.
    pub(super) handler_pos: Option<usize>,
}

/// Rows per table key, in file order.
pub(super) fn convention_rows() -> &'static std::collections::BTreeMap<&'static str, Vec<Convention>> {
    static ROWS: OnceLock<std::collections::BTreeMap<&'static str, Vec<Convention>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let mut out = std::collections::BTreeMap::new();
        for (key, text) in crate::testing::TABLE_FILES {
            let Some(doc) = trace_core::formats::jsonc::parse(text) else {
                continue;
            };
            let rows: Vec<Convention> = doc
                .get("syntax_conventions")
                .and_then(|v| v.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|r| {
                            let s = |k: &str| r.get(k).and_then(|v| v.as_str()).map(str::to_string);
                            let pos = |k: &str| {
                                r.get(k)
                                    .and_then(|v| v.get("pos"))
                                    .and_then(|v| v.as_u64())
                                    .and_then(|n| usize::try_from(n).ok())
                            };
                            Some(Convention {
                                rule: s("rule")?,
                                bridge: s("bridge").and_then(|b| b.parse::<BridgeKind>().ok()),
                                symbol: s("symbol"),
                                pattern: s("pattern"),
                                value: s("value"),
                                key_pos: pos("key"),
                                key_kw: r
                                    .get("key")
                                    .and_then(|v| v.get("kw"))
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                handler_pos: pos("handler"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.insert(key, rows);
        }
        out
    })
}

/// The rows that apply to files of `language`: its own table, plus the table of the language
/// whose code it calls directly without a binding layer (C++ calls C APIs).
pub(super) fn conventions(language: Language) -> Vec<&'static Convention> {
    let own = crate::languages::syntax(language).and_then(|s| s.library_table);
    let companion = match language {
        Language::Cpp => Some("c"),
        _ => None,
    };
    let rows = convention_rows();
    own.into_iter()
        .chain(companion)
        .filter_map(|k| rows.get(k))
        .flatten()
        .collect()
}

/// Last segment of a row symbol (`graphql-tag.gql` -> `gql`, `Napi::Object::Set` -> `Set`).
pub(super) fn last_segment(symbol: &str) -> &str {
    let cut = [
        symbol.rfind("::").map(|i| i + 2),
        symbol.rfind('.').map(|i| i + 1),
        symbol.rfind('#').map(|i| i + 1),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0);
    &symbol[cut..]
}

/// Segments of a row symbol (`Napi::Function::New` -> `[Napi, Function, New]`).
pub(super) fn symbol_segments(symbol: &str) -> Vec<&str> {
    symbol
        .split("::")
        .flat_map(|p| p.split(['.', '#']))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a written path (`lib.type`, or `type` imported by name) names `symbol`: the
/// whole dotted symbol, or its last segment written alone.
pub(super) fn symbol_written(symbol: &str, written: &[String]) -> bool {
    written.join(".") == symbol || (written.len() == 1 && written[0] == last_segment(symbol))
}

/// The text a `placeholder` of `pattern` stands for in `name` (`GreeterStub` under
/// `<Service>Stub` -> `Greeter`, `resolve_user` under `resolve_<field>` -> `user`); never
/// empty.
pub(super) fn placeholder_in(pattern: &str, placeholder: &str, name: &str) -> Option<String> {
    let (prefix, suffix) = pattern.split_once(placeholder)?;
    let value = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!value.is_empty()).then(|| value.to_string())
}
