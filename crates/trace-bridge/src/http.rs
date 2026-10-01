//! HTTP routes <-> clients (`http`, inferred when method + path template match exactly one
//! handler, else possible) and OpenAPI operations (`openapi`, contract match, inferred).
//!
//! Routes, clients and mounts come from `recognize` (channel effects derived from installed
//! package source, irreducible primitives, fs-route and reflection-root rows); their keys are
//! normalized here ([`normalize`]) with the placeholder families of the `route_patterns`
//! rows. Router mount points are resolved across files through the importing file's import
//! bindings. A route whose mount prefix stays unknown, and a client whose base URL is not a
//! literal, match by path suffix and are possible only.

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, FileId, Provider, Resolution, Tier};
use trace_library::table::{Section, Tables};
use trace_syntax::boundary::{Tpl, TplPart, PLACEHOLDER};

use crate::contracts::Operation;
use crate::ctx::{segments, Ctx, End, Fact};

/// HTTP methods (RFC 9110 §9 + PATCH, RFC 5789).
pub(crate) const VERBS: [&str; 9] =
    ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "TRACE", "CONNECT"];

/// One placeholder family of route keys (a `route_patterns` row).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    /// The whole segment (`*`, `**`).
    Exact(String),
    /// Delimited (`{name}`, `<name>`, `[name]`, `(optional)`) or prefixed (`:name`, `*name`:
    /// empty `close`).
    Wrap { open: String, close: String },
}

/// Placeholder families of route keys, read from the `route_patterns` rows of every table,
/// plus whether a key may start with its HTTP method (`METHOD /path` rows).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Placeholders {
    families: Vec<Family>,
    verb_prefix: bool,
}

impl Placeholders {
    pub(crate) fn from_tables(tables: &Tables) -> Placeholders {
        let mut out = Placeholders::default();
        for language in trace_core::Language::ALL {
            if trace_library::languages::table_language(language) != language {
                continue;
            }
            for row in tables.irreducible(language, Section::RoutePatterns) {
                if let Some(p) = row.pattern.as_deref() {
                    out.add(p);
                }
            }
        }
        out.families.sort();
        out.families.dedup();
        out
    }

    /// Read one pattern: the placeholder word is the first run of letters; the text before it
    /// opens the placeholder, the text after the last letter (without an optional `?` / `...`)
    /// closes it.
    fn add(&mut self, pattern: &str) {
        let pattern = pattern.trim();
        if let Some((first, _)) = pattern.split_once(' ') {
            if first.chars().all(|c| c.is_ascii_uppercase()) {
                self.verb_prefix = true;
            }
            return;
        }
        let Some(start) = pattern.find(|c: char| c.is_ascii_alphabetic()) else {
            if !pattern.is_empty() {
                self.families.push(Family::Exact(pattern.to_string()));
            }
            return;
        };
        let last = pattern.rfind(|c: char| c.is_ascii_alphanumeric()).unwrap_or(start);
        let open = pattern[..start].trim_end_matches("...").to_string();
        let close = pattern[last + 1..]
            .trim_start_matches("...")
            .trim_start_matches('?')
            .to_string();
        if open.is_empty() && close.is_empty() {
            return;
        }
        self.families.push(Family::Wrap { open, close });
    }

    /// Whether a route / URL segment is a placeholder.
    pub(crate) fn is_param(&self, seg: &str) -> bool {
        if seg.contains(MARK) || seg == PLACEHOLDER {
            return true;
        }
        if printf_verb(seg) {
            return true;
        }
        self.families.iter().any(|f| match f {
            Family::Exact(e) => seg == e,
            Family::Wrap { open, close } if close.is_empty() => {
                seg.len() > open.len() && seg.starts_with(open.as_str())
            }
            Family::Wrap { open, close } => match seg.find(open.as_str()) {
                Some(i) => seg[i + open.len()..].contains(close.as_str()),
                None => false,
            },
        })
    }
}

/// A printf-style conversion inside a segment (`%s`, `%d`, `%v`: C `printf` / Go `fmt`
/// format strings used to build URLs).
fn printf_verb(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.windows(2)
        .any(|w| w[0] == b'%' && matches!(w[1], b's' | b'd' | b'v' | b'q' | b'x' | b'f' | b'i' | b'u'))
}

const MARK: char = '\u{1}';

/// Normalized HTTP key of a template (SPEC §15a "Path normalization for HTTP").
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormPath {
    /// `/a/{}/b`.
    pub path: String,
    /// A non-literal part inside the path (or host).
    pub dynamic: bool,
    /// The template starts with a non-literal part (unknown base URL / mount prefix).
    pub dynamic_prefix: bool,
    /// A method written in front of the path (`GET /x` keys).
    pub verb: Option<String>,
}

/// Normalize a URL / route template. `require_root`: client URLs must be absolute paths or
/// URLs (relative URLs resolve against an unknown page and are skipped); route templates may
/// omit the leading slash.
pub(crate) fn normalize(tpl: &Tpl, require_root: bool, ph: &Placeholders) -> Option<NormPath> {
    let mut s = String::new();
    let mut dynamic = false;
    let mut dynamic_prefix = false;
    for part in &tpl.parts {
        match part {
            TplPart::Lit(t) => s.push_str(t),
            TplPart::Hole(name) if !name.is_empty() => s.push(MARK),
            TplPart::Hole(_) => {
                if s.is_empty() {
                    dynamic_prefix = true;
                } else {
                    s.push(MARK);
                    dynamic = true;
                }
            }
        }
    }
    let mut verb = None;
    if ph.verb_prefix {
        if let Some((first, rest)) = s.split_once(' ') {
            if VERBS.contains(&first) && rest.starts_with('/') {
                verb = Some(first.to_string());
                s = rest.to_string();
            }
        }
    }
    let lower = s.to_ascii_lowercase();
    let scheme_end = ["http://", "https://", "ws://", "wss://"]
        .iter()
        .find(|p| lower.starts_with(**p))
        .map(|p| p.len())
        .or_else(|| (require_root && s.starts_with("//")).then_some(2));
    if let Some(start) = scheme_end {
        let rest = &s[start..];
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        if host.contains(MARK) {
            dynamic = true;
        }
        s = path;
    } else if !dynamic_prefix && s.starts_with(MARK) {
        dynamic_prefix = true;
        s = s.trim_start_matches(MARK).to_string();
    }
    if require_root && !dynamic_prefix && !s.starts_with('/') {
        return None;
    }
    if let Some(i) = s.find(['?', '#']) {
        s.truncate(i);
    }
    let trimmed = s.trim_start_matches('^').trim_end_matches('$');
    let segs: Vec<String> = trimmed
        .split('/')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            if ph.is_param(seg) {
                PLACEHOLDER.to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    if segs.is_empty() && dynamic_prefix {
        return None;
    }
    Some(NormPath {
        path: format!("/{}", segs.join("/")),
        dynamic,
        dynamic_prefix,
        verb,
    })
}

/// Join two normalized paths.
pub(crate) fn join_paths(prefix: &str, path: &str) -> String {
    let a = prefix.trim_end_matches('/');
    let b = path.trim_start_matches('/');
    match (a.is_empty(), b.is_empty()) {
        (true, true) => "/".into(),
        (true, false) => format!("/{b}"),
        (false, true) => a.to_string(),
        (false, false) => format!("{a}/{b}"),
    }
}

/// HTTP method from a literal / constant spelling (`"post"`, `:get`, `HttpMethod.POST`,
/// `http.MethodPost`, `RequestMethod.GET`).
pub(crate) fn verb_of(spelling: &str) -> Option<String> {
    let last = spelling
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(spelling)
        .trim_matches(|c: char| c == '"' || c == '\'' || c == ':');
    let last = last.strip_prefix("Method").unwrap_or(last);
    let up = last.to_ascii_uppercase();
    VERBS.contains(&up.as_str()).then_some(up)
}

#[derive(Clone, Debug)]
struct Route {
    method: String,
    segs: Vec<String>,
    prefix_unknown: bool,
    dynamic: bool,
    case_insensitive: bool,
    end: End,
    handler: Option<String>,
    assumptions: Vec<String>,
}

#[derive(Clone, Debug)]
struct Client {
    method: String,
    segs: Vec<String>,
    dynamic: bool,
    dynamic_prefix: bool,
    /// A URL literal passed to a call (`recognize::url_literals`): matches only a route of
    /// exactly that full path.
    literal: bool,
    end: End,
}

#[derive(Clone, Debug)]
struct Mount {
    file: FileId,
    mounted: String,
    prefix: Vec<String>,
    dynamic: bool,
    receiver: Option<String>,
    receiver_state: String,
    /// Assumptions of a prefix resolved through a constant (`settings.API_V1_STR`).
    notes: Vec<String>,
    /// The dotted name a non-literal prefix came from, and the literal outer prefix.
    prefix_ref: Option<(String, String)>,
}

/// Resolve a mount prefix reference (`API_PREFIX`, `settings.API_V1_STR`) to a Python
/// string constant: a module-level constant of the file defining the name (through the
/// mounting file's import), or a class-body attribute of the class a module-level instance
/// was created from. Returns (value, assumption).
fn resolve_prefix_ref(
    ctx: &Ctx<'_>,
    file: FileId,
    dotted: &str,
    consts: &std::collections::HashMap<(FileId, String), String>,
    instances: &std::collections::HashMap<(FileId, String), String>,
) -> Option<(String, String)> {
    let segs: Vec<&str> = dotted.split('.').collect();
    if segs.is_empty() || segs.len() > 2 {
        return None;
    }
    let root = segs[0];
    let imported = ctx
        .file_facts(file)
        .is_some_and(|f| f.imports.iter().any(|i| i.local == root));
    let (def_file, name) = if imported {
        let (f2, rest) = ctx.resolve_import(file, root)?;
        match rest.as_slice() {
            [n] => (f2, n.clone()),
            _ => return None,
        }
    } else {
        (file, root.to_string())
    };
    let value = if segs.len() == 1 {
        consts.get(&(def_file, name))?.clone()
    } else {
        let class = instances.get(&(def_file, name))?;
        consts.get(&(def_file, format!("{class}.{}", segs[1])))?.clone()
    };
    let note = format!(
        "mount prefix `{dotted}` = \"{value}\" (the value written in {}; assumed not overridden at run time)",
        ctx.path(def_file)
    );
    Some((value, note))
}

pub(crate) fn span_of(s: &str) -> Option<trace_core::model::ByteSpan> {
    let (a, b) = s.split_once(':')?;
    Some(trace_core::model::ByteSpan::new(a.parse().ok()?, b.parse().ok()?))
}

/// Resolve the handler of a route registration to a symbol.
fn handler_end(ctx: &mut Ctx<'_>, f: &Fact<'_>, method: &str) -> Option<(End, Option<String>, Vec<String>)> {
    let at = f.at();
    let mut assumptions = Vec::new();
    let mut sym = f.fact.decl.and_then(|d| ctx.decl_symbol(f.file, d));
    if sym.is_none() {
        if let Some(span) = f.detail("handler_span").and_then(span_of) {
            sym = ctx.edge_target_in(f.file, span);
        }
    }
    if sym.is_none() {
        if let Some(q) = f.detail("handler_qualified") {
            let hits = ctx.qualified(q);
            if hits.len() == 1 {
                sym = Some(hits[0]);
            }
        }
    }
    if sym.is_none() {
        if let Some(h) = f.detail("handler") {
            sym = resolve_handler_text(ctx, f.file, h);
        }
    }
    let sym = match sym {
        Some(s) => s,
        None => {
            assumptions
                .push("the handler could not be resolved; the registering scope stands in for it".into());
            ctx.executing(f.file, f.fact.owner, f.fact.span.start)?
        }
    };
    // Class-based views dispatch on the HTTP method (`get`, `post`, ...).
    let s = ctx.index.symbol(sym);
    let sym = if s.kind.is_type() && method != "*" {
        let wanted = method.to_ascii_lowercase();
        ctx.children
            .get(&sym)
            .and_then(|kids| kids.iter().find(|k| ctx.index.symbol(**k).name == wanted).copied())
            .unwrap_or(sym)
    } else {
        sym
    };
    let name = ctx.index.symbol(sym).name.clone();
    Some((End { sym, at }, Some(name), assumptions))
}

pub(crate) fn resolve_handler_text(
    ctx: &Ctx<'_>,
    file: FileId,
    text: &str,
) -> Option<trace_core::model::SymbolId> {
    let unique = |v: Vec<trace_core::model::SymbolId>| (v.len() == 1).then(|| v[0]);
    if !text.contains('.') {
        if let Some(s) = unique(ctx.qualified_in(file, text)) {
            return Some(s);
        }
        // Same directory (Go package, sibling modules).
        let dir = crate::ctx::parent_dir(ctx.path(file));
        let siblings: Vec<_> = ctx
            .qualified(text)
            .into_iter()
            .filter(|s| crate::ctx::parent_dir(ctx.path(ctx.index.symbol(*s).file)) == dir)
            .collect();
        if let Some(s) = unique(siblings) {
            return Some(s);
        }
    }
    if let Some((f2, rest)) = ctx.resolve_import(file, text) {
        if !rest.is_empty() && rest != ["default"] {
            if let Some(s) = unique(ctx.qualified_in(f2, &rest.join("."))) {
                return Some(s);
            }
        }
    }
    unique(
        ctx.qualified(text)
            .into_iter()
            .filter(|s| {
                let k = ctx.index.symbol(*s).kind;
                k.is_callable() || k.is_type()
            })
            .collect(),
    )
}

fn resolve_mounted(ctx: &Ctx<'_>, m: &Mount) -> Option<(FileId, Option<String>)> {
    if let Some(module) = m.mounted.strip_prefix("module:") {
        return ctx.python_module_file(m.file, module).map(|f| (f, None));
    }
    let root = m.mounted.split('.').next()?;
    let imported = ctx
        .file_facts(m.file)
        .is_some_and(|f| f.imports.iter().any(|i| i.local == root));
    if imported {
        let (f2, rest) = ctx.resolve_import(m.file, &m.mounted)?;
        return Some(match rest.last().map(String::as_str) {
            None | Some("default") => (f2, None),
            Some(v) => (f2, Some(v.to_string())),
        });
    }
    Some((m.file, m.mounted.rsplit('.').next().map(str::to_string)))
}

/// Mounts resolved once per detection: target file -> (mount, the mounted variable).
struct Mounts<'m> {
    by_target: std::collections::HashMap<FileId, Vec<(&'m Mount, Option<String>)>>,
}

impl<'m> Mounts<'m> {
    fn new(ctx: &Ctx<'_>, mounts: &'m [Mount]) -> Self {
        let mut by_target: std::collections::HashMap<FileId, Vec<(&'m Mount, Option<String>)>> =
            std::collections::HashMap::new();
        for m in mounts {
            if let Some((target, var)) = resolve_mounted(ctx, m) {
                by_target.entry(target).or_default().push((m, var));
            }
        }
        Mounts { by_target }
    }
}

/// Mount prefixes of routes registered on `var` in `file` (`None` = file-level: the module is
/// mounted as a whole, `include("app.urls")`):
/// `(prefix segments, prefix fully known)`.
fn mount_prefixes(mounts: &Mounts<'_>, file: FileId, var: Option<&str>, depth: u32) -> Vec<Prefix> {
    if depth > 8 {
        return vec![(Vec::new(), false, Vec::new())];
    }
    let mut out = Vec::new();
    for (m, target_var) in mounts.by_target.get(&file).map(Vec::as_slice).unwrap_or_default() {
        let (target, m) = (file, *m);
        let matches = match (&target_var, var) {
            (None, _) => true,
            (Some(t), Some(v)) => t == v,
            (Some(_), None) => false,
        };
        if !matches || (target == m.file && m.receiver.as_deref() == var && var.is_some()) {
            continue;
        }
        if m.dynamic {
            out.push((m.prefix.clone(), false, m.notes.clone()));
            continue;
        }
        match m.receiver_state.as_str() {
            // A derived mount: the receiver is itself mounted when some mount targets it.
            "derived" => {
                let outer = mount_prefixes(mounts, m.file, m.receiver.as_deref(), depth + 1);
                if outer.is_empty() {
                    out.push((m.prefix.clone(), true, m.notes.clone()));
                }
                for (mut p, known, mut notes) in outer {
                    p.extend(m.prefix.iter().cloned());
                    notes.extend(m.notes.iter().cloned());
                    out.push((p, known, notes));
                }
            }
            "router" => {
                let outer = mount_prefixes(mounts, m.file, m.receiver.as_deref(), depth + 1);
                if outer.is_empty() {
                    out.push((m.prefix.clone(), false, m.notes.clone()));
                }
                for (mut p, known, mut notes) in outer {
                    p.extend(m.prefix.iter().cloned());
                    notes.extend(m.notes.iter().cloned());
                    out.push((p, known, notes));
                }
            }
            _ => out.push((m.prefix.clone(), true, m.notes.clone())),
        }
        if out.len() > 4 * MAX_MOUNT_PREFIXES {
            out.sort();
            out.dedup();
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A mount prefix: its segments, whether it is fully known, and the assumptions it rests on.
type Prefix = (Vec<String>, bool, Vec<String>);

/// Distinct mount prefixes a route may take before its prefix counts as unknown.
const MAX_MOUNT_PREFIXES: usize = 64;

fn seg_eq(a: &str, b: &str, ci: bool) -> bool {
    a == "{}" || b == "{}" || if ci { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// Match `client` against `route` segments; `*_open` = unknown prefix (suffix matching).
/// Returns the number of parameter positions used (lower = more specific).
fn path_match(
    client: &[String],
    client_open: bool,
    route: &[String],
    route_open: bool,
    ci: bool,
) -> Option<usize> {
    let (short, long, ok) = match (client_open, route_open) {
        (false, false) => (client, route, client.len() == route.len()),
        (false, true) => (route, client, client.len() >= route.len()),
        (true, false) => (client, route, route.len() >= client.len()),
        (true, true) => {
            if client.len() <= route.len() {
                (client, route, true)
            } else {
                (route, client, true)
            }
        }
    };
    if !ok || (short.is_empty() && !long.is_empty() && (client_open || route_open)) {
        return None;
    }
    let offset = long.len() - short.len();
    let mut wild = 0;
    for (i, s) in short.iter().enumerate() {
        let l = &long[offset + i];
        if !seg_eq(s, l, ci) {
            return None;
        }
        if (s == "{}") != (l == "{}") {
            wild += 1;
        }
    }
    Some(wild)
}

fn method_ok(client: &str, route: &str) -> bool {
    client == route || route == "*" || client == "*"
}

fn id_matches(op_id: &str, name: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    op_id == name
        || op_id.starts_with(&format!("{name}_"))
        || op_id.split('-').any(|p| p == name)
        || norm(op_id) == norm(name)
}

fn path_label(method: &str, segs: &[String]) -> String {
    format!("{method} /{}", segs.join("/"))
}

pub(crate) fn detect(ctx: &mut Ctx<'_>, operations: &[Operation]) {
    let facts = ctx.of(BridgeKind::Http, BoundaryRole::Provides);
    let mounts: Vec<Mount> = facts
        .iter()
        .filter(|f| f.flag("mount"))
        .filter_map(|f| {
            Some(Mount {
                file: f.file,
                mounted: f.detail("mounted")?.to_string(),
                prefix: segments(f.detail("prefix").unwrap_or("/")),
                dynamic: f.flag("dynamic"),
                receiver: f.detail("receiver").map(str::to_string),
                receiver_state: f.detail("receiver_state").unwrap_or("known").to_string(),
                notes: Vec::new(),
                prefix_ref: f
                    .detail("prefix_ref")
                    .map(|r| (r.to_string(), f.detail("prefix_base").unwrap_or("/").to_string())),
            })
        })
        .collect();
    // Python path constants and module-level instances (trace-syntax `CONST` / `INSTANCE`).
    let mut consts: std::collections::HashMap<(FileId, String), String> = std::collections::HashMap::new();
    let mut instances: std::collections::HashMap<(FileId, String), String> = std::collections::HashMap::new();
    for f in &facts {
        if f.flag("const") {
            if let (Some(k), Some(v)) = (f.name().strip_prefix("CONST "), f.detail("value")) {
                consts.insert((f.file, k.to_string()), v.to_string());
            }
        } else if f.flag("instance") {
            if let (Some(k), Some(c)) = (f.name().strip_prefix("INSTANCE "), f.detail("class")) {
                instances.insert((f.file, k.to_string()), c.to_string());
            }
        }
    }
    let mut mounts = mounts;
    if !consts.is_empty() {
        for m in mounts.iter_mut() {
            let Some((dotted, base)) = m.prefix_ref.clone() else { continue };
            if !m.dynamic {
                continue;
            }
            if let Some((value, note)) = resolve_prefix_ref(ctx, m.file, &dotted, &consts, &instances) {
                let mut prefix = segments(&base);
                prefix.extend(segments(&value));
                m.prefix = prefix;
                m.dynamic = false;
                m.notes.push(note);
            }
        }
    }
    let resolved_mounts = Mounts::new(ctx, &mounts);
    let module_mounts = mounts.iter().any(|m| m.mounted.starts_with("module:"));
    // (file, router variable) -> distinct mount prefixes (shared by that router's routes).
    let mut prefix_memo: std::collections::HashMap<(FileId, Option<String>), Vec<Prefix>> =
        std::collections::HashMap::new();
    let mut routes: Vec<Route> = Vec::new();
    for f in facts.iter().filter(|f| !f.flag("mount")) {
        let Some(method) = f.detail("method").map(str::to_string) else { continue };
        let Some(path) = f.detail("path") else { continue };
        let Some((end, handler, mut assumptions)) = handler_end(ctx, f, &method) else { continue };
        let framework = f.detail("framework").unwrap_or("route table").to_string();
        assumptions.insert(0, format!("{framework} route registration; method and path template match"));
        let segs = segments(path);
        let var = f.detail("router");
        let is_router = f.flag("prefix_unknown");
        let receiver_unknown = f.flag("receiver_unknown");
        // A registration on no receiver object belongs to its module; in a repository that
        // mounts whole modules, the module's mount decides its prefix.
        let file_level = var.is_none() && module_mounts;
        let mut prefixes = if is_router || receiver_unknown || file_level {
            prefix_memo
                .entry((f.file, var.map(str::to_string)))
                .or_insert_with(|| {
                    let mut p = mount_prefixes(&resolved_mounts, f.file, var, 0);
                    p.sort();
                    p.dedup();
                    p
                })
                .clone()
        } else {
            Vec::new()
        };
        // Too many distinct mount chains: the prefix is treated as unknown (suffix match).
        let too_many = prefixes.len() > MAX_MOUNT_PREFIXES;
        if too_many {
            prefixes.clear();
        }
        let base = Route {
            method: method.clone(),
            segs: segs.clone(),
            prefix_unknown: is_router || too_many,
            dynamic: f.flag("dynamic"),
            case_insensitive: f.flag("case_insensitive"),
            end,
            handler,
            assumptions,
        };
        if prefixes.is_empty() {
            let mut r = base;
            if file_level {
                // A module that is not the target of a visible module mount and
                // includes nothing itself may be mounted anywhere.
                let includes_others = mounts.iter().any(|m| m.file == f.file);
                if !includes_others {
                    r.prefix_unknown = true;
                }
            }
            if too_many {
                r.assumptions.push(format!(
                    "more than {MAX_MOUNT_PREFIXES} distinct mount prefixes; matched by path suffix"
                ));
            } else if r.prefix_unknown {
                r.assumptions
                    .push("the router's mount prefix is not visible; matched by path suffix".into());
            } else if receiver_unknown {
                r.assumptions.push(format!(
                    "routes registered on `{}` are assumed to be mounted at the root",
                    var.unwrap_or("?")
                ));
            }
            routes.push(r);
        } else {
            for (prefix, known, notes) in prefixes {
                let mut r = base.clone();
                let mut full = prefix.clone();
                full.extend(segs.iter().cloned());
                r.segs = full;
                r.prefix_unknown = !known;
                r.assumptions.push(if known {
                    format!("mounted under /{}", prefix.join("/"))
                } else {
                    "part of the mount prefix is not a literal; matched by path suffix".into()
                });
                r.assumptions.extend(notes);
                routes.push(r);
            }
        }
    }
    if routes.is_empty() {
        return;
    }
    let mut clients: Vec<Client> = Vec::new();
    for f in ctx.of(BridgeKind::Http, BoundaryRole::Uses) {
        if f.flag("unmatched") {
            continue;
        }
        let (Some(method), Some(path)) = (f.detail("method"), f.detail("path")) else { continue };
        let Some(end) = ctx.end_of(&f) else { continue };
        clients.push(Client {
            method: method.to_string(),
            segs: segments(path),
            dynamic: f.flag("dynamic"),
            dynamic_prefix: f.flag("dynamic_prefix"),
            literal: f.flag("url_literal"),
            end,
        });
    }
    // Routes bucketed by segment count (a closed client matches closed routes of its own
    // length only); candidates keep route order.
    let mut closed_by_len: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    let mut open_routes: Vec<usize> = Vec::new();
    for (i, r) in routes.iter().enumerate() {
        if r.prefix_unknown {
            open_routes.push(i);
        } else {
            closed_by_len.entry(r.segs.len()).or_default().push(i);
        }
    }
    for c in &clients {
        if openapi_match(ctx, c, &routes, operations) {
            continue;
        }
        let mut order: Vec<usize> = if c.literal {
            Vec::new()
        } else {
            open_routes.clone()
        };
        if c.dynamic_prefix {
            for (len, list) in &closed_by_len {
                if *len >= c.segs.len() {
                    order.extend(list.iter().copied());
                }
            }
        } else if let Some(list) = closed_by_len.get(&c.segs.len()) {
            order.extend(list.iter().copied());
        }
        order.sort_unstable();
        let mut cands: Vec<(usize, &Route)> = order
            .iter()
            .map(|&i| &routes[i])
            .filter(|r| method_ok(&c.method, &r.method))
            .filter(|r| !c.literal || (!r.prefix_unknown && !r.dynamic))
            .filter_map(|r| {
                path_match(&c.segs, c.dynamic_prefix, &r.segs, r.prefix_unknown, r.case_insensitive)
                    .map(|w| (w, r))
            })
            .collect();
        if cands.is_empty() {
            continue;
        }
        // A URL literal without a method names one route only when its path has one route.
        if c.literal && c.method == "*" && cands.len() > 1 {
            continue;
        }
        let exact = !c.dynamic && !c.dynamic_prefix;
        let mut extra = Vec::new();
        if cands.len() > 1 && exact && cands.iter().all(|(_, r)| !r.prefix_unknown) {
            // Static segments take precedence over parameters (`/users/me` vs `/users/{}`)
            // when exactly one candidate is the most specific.
            let best = cands.iter().map(|(w, _)| *w).min().unwrap_or(0);
            let top: Vec<(usize, &Route)> = cands.iter().filter(|(w, _)| *w == best).cloned().collect();
            if top.len() == 1 {
                cands = top;
                extra.push("a static path segment takes precedence over a parameter".to_string());
            }
        }
        let unique = cands.len() == 1;
        let tier = if unique && exact && !cands[0].1.prefix_unknown && !cands[0].1.dynamic {
            Tier::Inferred
        } else {
            Tier::Possible
        };
        let mut assumptions = cands[0].1.assumptions.clone();
        if c.dynamic_prefix {
            assumptions.push("the client's base URL is not a literal; matched by path suffix".into());
        } else if c.dynamic {
            assumptions.push("the URL contains non-literal parts".into());
        }
        assumptions.extend(extra);
        let method = if cands[0].1.method == "*" {
            c.method.clone()
        } else {
            cands[0].1.method.clone()
        };
        let label = path_label(&method, if unique { &cands[0].1.segs } else { &c.segs });
        let ends: Vec<End> = cands.iter().map(|(_, r)| r.end).collect();
        ctx.emit(
            BridgeKind::Http,
            c.end,
            &ends,
            tier,
            Provider::Contract("http".into()),
            Resolution::RouteMatch,
            &label,
            &assumptions,
            None,
        );
    }
}

/// OpenAPI: a client and a handler that match the same declared operation. Returns true
/// when the contract decided the client (no plain HTTP rows are added then).
fn openapi_match(ctx: &mut Ctx<'_>, c: &Client, routes: &[Route], operations: &[Operation]) -> bool {
    if operations.is_empty() {
        return false;
    }
    let ops: Vec<&Operation> = operations
        .iter()
        .filter(|op| method_ok(&c.method, &op.method))
        .filter(|op| path_match(&c.segs, c.dynamic_prefix, &op.full, false, false).is_some())
        .collect();
    if ops.is_empty() {
        return false;
    }
    let mut pairs: Vec<(&Operation, &Route)> = Vec::new();
    for op in &ops {
        let mut rs: Vec<&Route> = routes
            .iter()
            .filter(|r| method_ok(&op.method, &r.method))
            .filter(|r| {
                path_match(&op.full, false, &r.segs, r.prefix_unknown, r.case_insensitive).is_some()
                    || (op.relative.len() != op.full.len()
                        && path_match(&op.relative, false, &r.segs, r.prefix_unknown, r.case_insensitive)
                            .is_some())
            })
            .collect();
        if rs.len() > 1 {
            if let Some(id) = &op.operation_id {
                let named: Vec<&Route> = rs
                    .iter()
                    .copied()
                    .filter(|r| r.handler.as_deref().is_some_and(|h| id_matches(id, h)))
                    .collect();
                if !named.is_empty() {
                    rs = named;
                }
            }
        }
        pairs.extend(rs.into_iter().map(|r| (*op, r)));
    }
    if pairs.is_empty() {
        return false;
    }
    let unique = pairs.len() == 1 && ops.len() == 1;
    let (op, route) = pairs[0];
    // With an unknown base URL the literal part must be the whole declared path.
    let whole_path = !c.dynamic_prefix || c.segs.len() == op.full.len();
    let mut assumptions = vec![format!(
        "declared by the OpenAPI operation {} /{}{}",
        op.method,
        op.full.join("/"),
        op.operation_id
            .as_deref()
            .map(|i| format!(" ({i})"))
            .unwrap_or_default()
    )];
    assumptions.extend(route.assumptions.iter().skip(1).cloned());
    if c.dynamic_prefix {
        assumptions
            .push("the client's base URL is not a literal; the contract path matched its suffix".into());
    }
    let ends: Vec<End> = pairs.iter().map(|(_, r)| r.end).collect();
    let contract = if pairs.iter().all(|(o, _)| o.file == op.file) {
        Some(op.file)
    } else {
        None
    };
    let label = format!(
        "{}{}",
        path_label(&op.method, &op.full),
        op.operation_id
            .as_deref()
            .map(|i| format!(" ({i})"))
            .unwrap_or_default()
    );
    ctx.emit(
        BridgeKind::Openapi,
        c.end,
        &ends,
        if unique && !c.dynamic && whole_path {
            Tier::Inferred
        } else {
            Tier::Possible
        },
        Provider::Contract("openapi".into()),
        Resolution::ContractMatch,
        &label,
        &assumptions,
        contract,
    );
    true
}

#[cfg(test)]
#[path = "../tests/unit/http.rs"]
mod tests;
