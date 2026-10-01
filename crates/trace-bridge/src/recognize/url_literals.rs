//! URL-literal clients (rule 10) and the literal templates of keys: a call without channel
//! effects passing a fully written URL / absolute path is a candidate HTTP client of that
//! path; command-line words that name files; literal prefixes of templates.

use super::*;

impl Recognizer<'_, '_> {
    /// URL literals (the client side by key): a call passing a string that is an absolute
    /// path or a URL (`client.get({url: "/api/v1/items/"})`, `requests.get("https://h/x")`)
    /// is a candidate HTTP client of that path. The matcher links it only to a route of
    /// exactly that full path (`url_literal`). The method is the HTTP method token the callee
    /// is named after, else one passed as a literal argument (`method: "POST"`), else any.
    /// Calls with channel effects (registrations, derived clients) are left to those.
    pub(super) fn url_literals(
        &self,
        site: &Site<'_>,
        calls: &[(u32, String, Vec<Effect>, bool)],
        out: &mut Vec<BoundaryFact>,
    ) {
        let effectful: BTreeSet<u32> = calls.iter().map(|(s, _, _, _)| *s).collect();
        for (ci, call) in site.facts.calls.iter().enumerate() {
            if effectful.contains(&call.callee_span.start) {
                continue;
            }
            let Some(detail) = site.facts.call_detail(ci) else { continue };
            if !detail.arguments.iter().any(|a| a.has_string) {
                continue;
            }
            // A call passing a function registers it (a route, a listener), never requests a
            // URL; a decorator call decorates a declaration.
            let passes_function = site.facts.callbacks.iter().any(|cb| cb.call_callee_span == call.callee_span)
                || site.facts.anonymous.iter().any(|a| {
                    matches!(&a.consumer, trace_core::facts::Consumer::Argument { call: c, .. } if *c as usize == ci)
                });
            if passes_function
                || site.parsed.applied_to(call.callee_span).is_some()
                || decorated_declaration(site.facts, call.span).is_some()
                || !url_client_call(site, call)
            {
                continue;
            }
            let strings = site.parsed.call_strings(call.callee_span, &|_| None);
            let method = call
                .member
                .as_deref()
                .and_then(trace_library::channels::http_method_token)
                .map(str::to_string)
                .or_else(|| {
                    strings
                        .iter()
                        .filter_map(|s| s.value.template.as_ref().and_then(Tpl::plain))
                        .find_map(|t| trace_library::channels::http_method_token(&t).map(str::to_string))
                })
                .unwrap_or_else(|| "*".to_string());
            for s in &strings {
                let Some(tpl) = s.value.template.as_ref() else { continue };
                let Some(norm) = normalize(tpl, true, &self.placeholders) else { continue };
                // Only a fully written path names a route: no unknown base, at least one
                // literal segment.
                if norm.dynamic_prefix
                    || !norm
                        .path
                        .split('/')
                        .any(|seg| !seg.is_empty() && seg != trace_syntax::boundary::PLACEHOLDER)
                {
                    continue;
                }
                let mut detail = vec![
                    ("method".to_string(), method.clone()),
                    ("path".to_string(), norm.path.clone()),
                    ("url_literal".to_string(), "true".to_string()),
                    ("via".to_string(), "url literal".to_string()),
                ];
                if norm.dynamic {
                    detail.push(("dynamic".into(), "true".into()));
                }
                out.push(fact(
                    site,
                    BridgeKind::Http,
                    BoundaryRole::Uses,
                    format!("{method} {}", norm.path),
                    call.owner,
                    None,
                    call.span,
                    detail,
                ));
            }
        }
    }
}

/// Whether the language server's view of `call` allows a URL-literal client: the file has
/// semantic answers (without them nothing tells a request from a container read), and the call
/// is not a read of the language's own standard-library container (`Map.get("/users")`,
/// `dict.get("/users")`: a standard-library declaration named like a container read method of
/// the language's adapter).
fn url_client_call(site: &Site<'_>, call: &CallSite) -> bool {
    let Some(sem) = site.semantics else {
        return false;
    };
    let Some(lc) = sem
        .library_calls
        .iter()
        .find(|l| call.callee_span.start <= l.at.start && l.at.end <= call.callee_span.end)
    else {
        return true;
    };
    let stdlib = sem.library_files.get(lc.file as usize).is_some_and(|f| f.stdlib);
    let container_read = call.member.as_deref().is_some_and(|m| {
        trace_library::languages::adapter(site.rec.language).is_some_and(|s| s.read_methods.contains(&m))
    });
    !(stdlib && container_read)
}

/// The literal text before the first unknown part of a template.
pub(super) fn literal_prefix(t: &Tpl) -> String {
    let mut s = String::new();
    for p in &t.parts {
        match p {
            TplPart::Lit(l) => s.push_str(l),
            TplPart::Hole(_) => break,
        }
    }
    s
}

/// Words of a command line template (unknown parts split words).
pub(super) fn command_tokens(t: &Tpl) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut broken = false;
    for p in &t.parts {
        match p {
            TplPart::Lit(l) => {
                for ch in l.chars() {
                    if ch.is_whitespace() {
                        if !current.is_empty() && !broken {
                            out.push(std::mem::take(&mut current));
                        }
                        current.clear();
                        broken = false;
                    } else {
                        current.push(ch);
                    }
                }
            }
            TplPart::Hole(_) => broken = true,
        }
    }
    if !current.is_empty() && !broken {
        out.push(current);
    }
    out.into_iter()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// A command-line word that can name a file: a path, or a name with an extension.
pub(super) fn names_a_file(token: &str) -> bool {
    if token.starts_with('-') || token.contains("://") {
        return false;
    }
    if token.contains('/') {
        return true;
    }
    match token.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// Rule 10: a URL or an absolute path.
pub(super) fn looks_like_url(t: &Tpl) -> bool {
    let head = literal_prefix(t).to_ascii_lowercase();
    head.starts_with('/')
        || head.starts_with("http://")
        || head.starts_with("https://")
        || head.starts_with("ws://")
        || head.starts_with("wss://")
}

#[cfg(test)]
#[path = "../../tests/unit/recognize/url_literals.rs"]
mod tests;
