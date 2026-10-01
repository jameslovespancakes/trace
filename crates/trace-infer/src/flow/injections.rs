//! By-name injection rules of the value flow: active `runtime_dispatch` rows with pattern
//! [`INJECT_BY_PARAMETER_NAME`] and the values they bind (child of [`crate::flow`]).

use super::*;

pub use trace_library::injected::INJECT_BY_PARAMETER_NAME;

/// One active by-name injection rule (a `runtime_dispatch` row with pattern
/// [`INJECT_BY_PARAMETER_NAME`] whose `activated_by` package is an installed dependency).
///
/// The runtime calls every *provider* (a callable decorated with `decorator`) and passes its
/// value to each parameter of a *consumer* (a test declaration or another provider in a test
/// file or a `shared_file`) that has the provider's name: a provider of the same class, else
/// of the same file, else of the nearest `shared_file` in the consumer's directory or its
/// ancestors (a provider never receives itself: `def app(app)` requests the outer one).
/// A provider's value is what it returns, or what it yields when it is a generator (the
/// runtime runs it up to its `yield`). Providers requesting providers chain. A parameter no
/// repository provider serves receives the value of the one provider of its name that
/// installed packages declare (`installed`, rule "provider of an installed plugin").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Injection {
    pub language: Language,
    /// Decorator spelling of providers (`symbol` of the row): matched on the decorator head
    /// without arguments, as written or by its last dotted segment.
    pub decorator: String,
    /// File name whose providers are visible to its directory subtree (`glob` of the row).
    pub shared_file: Option<String>,
    /// Keyword of the provider decorator that renames what it provides (the row's `key`
    /// selector `{"kw": k}`): `@provider(k="client") def make_client()` provides `client`.
    pub name_keyword: Option<String>,
    /// Provided name -> library class of the value of the only installed provider of that
    /// name (`LibraryKnowledge::providers`, [`trace_library::injected`]).
    pub installed: BTreeMap<String, String>,
}

/// The active by-name injection rules among `rows` (language, row): pattern
/// [`INJECT_BY_PARAMETER_NAME`], a provider decorator, and an `activated_by` package that
/// `installed(language, package)` reports as an installed dependency. A row without
/// `activated_by` is a language rule and always active.
pub(crate) fn injections<'r>(
    rows: impl IntoIterator<Item = (Language, &'r IrreducibleRow)>,
    installed: &dyn Fn(Language, &str) -> bool,
) -> Vec<Injection> {
    let mut out: Vec<Injection> = rows
        .into_iter()
        .filter(|(_, r)| r.pattern.as_deref() == Some(INJECT_BY_PARAMETER_NAME))
        .filter(|(language, r)| r.activated_by.as_deref().is_none_or(|p| installed(*language, p)))
        .filter_map(|(language, r)| {
            let decorator = r.symbol.clone().filter(|s| !s.trim().is_empty())?;
            let name_keyword = match r.key_sel() {
                Ok(Some(RowSel::Arg(ArgSel::Kw(k) | ArgSel::PosOrKw(_, k)))) => Some(k),
                _ => None,
            };
            Some(Injection {
                language,
                decorator,
                shared_file: r.glob.clone().filter(|g| !g.trim().is_empty()),
                name_keyword,
                installed: BTreeMap::new(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (a.language, &a.decorator, &a.shared_file).cmp(&(b.language, &b.decorator, &b.shared_file))
    });
    out.dedup();
    out
}

/// Active injection rules of the languages present in `index`, from the `runtime_dispatch`
/// rows of the embedded tables, gated on the installed packages.
pub(crate) fn index_injections(index: &Index, library: &LibraryInputs<'_>) -> Vec<Injection> {
    let languages: BTreeSet<Language> = index.files.iter().map(|f| f.language).collect();
    let rows = languages.iter().flat_map(|&l| {
        library
            .tables
            .irreducible(l, Section::RuntimeDispatch)
            .iter()
            .map(move |r| (l, r))
    });
    let installed = |language: Language, package: &str| {
        trace_env::EcosystemId::of_language(language).is_some_and(|e| library.installed.contains(e, package))
    };
    let mut rules = injections(rows, &installed);
    for rule in &mut rules {
        rule.installed = installed_values(rule, library.knowledge);
    }
    rules
}

/// Provided name -> value class of the only provider installed packages declare for it
/// (Python; names with several installed providers or none whose value is known are left out).
pub(super) fn installed_values(rule: &Injection, knowledge: &LibraryKnowledge) -> BTreeMap<String, String> {
    let Some(providers) = knowledge.providers.get(&rule.decorator) else {
        return BTreeMap::new();
    };
    providers
        .iter()
        .filter_map(|(name, list)| match list.as_slice() {
            [only] => Some((name.clone(), only.value.clone()?)),
            _ => None,
        })
        .collect()
}

impl Injection {
    /// Whether a decorator head (without arguments) names this rule's provider decorator.
    fn names_decorator(&self, head: &str) -> bool {
        trace_library::injected::names_decorator(&self.decorator, head)
    }

    /// Whether one of `s`'s decorator texts names this rule's provider decorator.
    pub(super) fn provides(&self, s: &Symbol) -> bool {
        s.decorators.iter().any(|d| {
            let head = d.trim_start_matches('@').split('(').next().unwrap_or_default().trim();
            self.names_decorator(head)
        })
    }

    /// The name provider `s` provides: the literal its provider decorator call passes under
    /// the rule's renaming keyword (`@provider(name="client")`, from the lowered decorator
    /// expression of the `Decorated` fact), else its own name.
    fn provided_name(&self, index: &Index, s: &Symbol) -> String {
        self.rename(index, s).unwrap_or_else(|| s.name.clone())
    }

    fn rename(&self, index: &Index, s: &Symbol) -> Option<String> {
        let keyword = self.name_keyword.as_deref()?;
        let facts = index.file(s.file).facts.as_ref()?;
        let decorators = facts.flow.iter().find_map(|f| match f {
            FlowFact::Decorated {
                function, decorators, ..
            } if *function == s.decl => Some(decorators),
            _ => None,
        })?;
        decorators.iter().find_map(|d| {
            let Expr::Call { func, kwargs, .. } = d else {
                return None;
            };
            if !self.names_decorator(&dotted(func)?) {
                return None;
            }
            kwargs.iter().find_map(|(k, v)| match v {
                Expr::Name { name, .. } if k == keyword => name
                    .strip_prefix(LITERAL_PREFIX)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
                _ => None,
            })
        })
    }
}

/// Dotted text of a name / attribute chain (`pkg.fixture`).
fn dotted(e: &Expr) -> Option<String> {
    match e {
        Expr::Name { name, .. } => Some(name.clone()),
        Expr::Attr { object, attr, .. } => Some(format!("{}.{attr}", dotted(object)?)),
        _ => None,
    }
}

/// Bindings of every active by-name injection rule ([`Injection`]). Each binding is written
/// to the consumer parameter's default slot from the consumer's file scope: a call of the
/// chosen provider with no arguments (the parameter holds the provider's return values), or
/// for a generator provider the values it yields (`Node::Yielded`). Providers are matched
/// by the name they provide ([`Injection::provided_name`]). A parameter no repository
/// provider serves and that an installed provider serves ([`Injection::installed`]) holds a
/// library object of that provider's value class (interned in `libraries`).
pub(super) fn injection_binds(
    index: &Index,
    names: &mut Interner,
    selfs: &HashMap<SymbolId, SelfParam>,
    rules: &[Injection],
    libraries: &mut Vec<String>,
    library_ids: &mut HashMap<String, u32>,
) -> Vec<Constraint> {
    if rules.is_empty() {
        return Vec::new();
    }
    let test_files = test_files(index);
    let dir_of = |path: &str| trace_core::relpath::parent(path).to_string();
    let file_name = |path: &str| path.rsplit('/').next().unwrap_or_default().to_string();
    let top_level = |s: &Symbol| s.parent.is_none_or(|p| index.symbol(p).is_synthetic());
    let mut out = Vec::new();
    let mut providers_total = 0usize;
    for rule in rules {
        let shared = |f: FileId| {
            rule.shared_file
                .as_deref()
                .is_some_and(|name| file_name(index.file_path(f)) == name)
        };
        let in_scope =
            |f: FileId| index.file(f).language == rule.language && (test_files[f.idx()] || shared(f));
        // provided name -> providers (symbol order).
        let mut providers: HashMap<String, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if s.kind.is_callable() && in_scope(s.file) && rule.provides(s) {
                providers.entry(rule.provided_name(index, s)).or_default().push(s.id);
            }
        }
        if providers.is_empty() {
            continue;
        }
        providers_total += providers.values().map(Vec::len).sum::<usize>();
        let pick = |consumer: &Symbol, name: &str| -> Option<SymbolId> {
            let found = providers.get(name)?;
            let others = || found.iter().copied().filter(|&f| f != consumer.id);
            if let Some(parent) = consumer.parent.filter(|p| !index.symbol(*p).is_synthetic()) {
                if let Some(f) = others().find(|&f| index.symbol(f).parent == Some(parent)) {
                    return Some(f);
                }
            }
            if let Some(f) = others().find(|&f| {
                let s = index.symbol(f);
                s.file == consumer.file && top_level(s)
            }) {
                return Some(f);
            }
            let dir = dir_of(index.file_path(consumer.file));
            others()
                .filter(|&f| {
                    let s = index.symbol(f);
                    let path = index.file_path(s.file);
                    top_level(s) && s.file != consumer.file && shared(s.file) && {
                        let d = dir_of(path);
                        d.is_empty() || dir == d || dir.starts_with(&format!("{d}/"))
                    }
                })
                .max_by_key(|&f| (dir_of(index.file_path(index.symbol(f).file)).len(), std::cmp::Reverse(f)))
        };
        for s in &index.symbols {
            if !s.kind.is_callable() || s.is_synthetic() || !in_scope(s.file) {
                continue;
            }
            if !s.is_test && !rule.provides(s) {
                continue;
            }
            let receiver = selfs.get(&s.id).map(|sp| sp.name);
            for p in &s.parameters {
                let name = names.intern(p);
                if Some(name) == receiver {
                    continue;
                }
                let Some(provider) = pick(s, p) else {
                    if let Some(class) = rule.installed.get(p.as_str()) {
                        let id = *library_ids.entry(class.clone()).or_insert_with(|| {
                            libraries.push(class.clone());
                            libraries.len() as u32 - 1
                        });
                        out.push(Constraint {
                            rule: Rule::Bind {
                                target: Target::Var(ScopeKey::Symbol(s.id), name),
                                value: installed_value(id),
                            },
                            scope: ScopeKey::Module(s.file),
                            file: s.file,
                            test: false,
                        });
                    }
                    continue;
                };
                let generator = matches!(
                    index.symbol(provider).execution,
                    ExecutionModel::Generator | ExecutionModel::AsyncGenerator
                );
                let value = if generator {
                    // The runtime runs a generator provider up to its `yield`: the parameter
                    // receives the yielded value.
                    Node::Yielded {
                        function: provider,
                        name: names.intern(YIELDED),
                    }
                } else {
                    Node::Call(Box::new(CallNode {
                        func: Node::Name {
                            name,
                            target: Some(provider),
                            span: NOWHERE,
                        },
                        func_span: NOWHERE,
                        span: NOWHERE,
                        args: Vec::new(),
                        kwargs: Vec::new(),
                        effects: Vec::new(),
                        not_identical: Vec::new(),
                        library: None,
                    }))
                };
                out.push(Constraint {
                    rule: Rule::Bind {
                        target: Target::Var(ScopeKey::Symbol(s.id), name),
                        value,
                    },
                    scope: ScopeKey::Module(s.file),
                    file: s.file,
                    test: false,
                });
            }
        }
    }
    if trace_core::env::profile() {
        eprintln!("profile-flow: injection providers={providers_total} bindings={}", out.len());
    }
    out
}

/// The value an installed provider passes: a library object of its value class (library
/// `id`), made by no repository code (a synthetic library call without arguments).
fn installed_value(id: u32) -> Node {
    Node::Call(Box::new(CallNode {
        func: Node::Opaque,
        func_span: NOWHERE,
        span: NOWHERE,
        args: Vec::new(),
        kwargs: Vec::new(),
        effects: Vec::new(),
        not_identical: Vec::new(),
        library: Some(id),
    }))
}

#[cfg(test)]
#[path = "../../tests/unit/flow/injections.rs"]
mod tests;
