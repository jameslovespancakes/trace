//! Filesystem routing (`fs_routes` rows): a file below the routes directory of an installed
//! routing convention is a route whose key comes from its path.

use super::*;

impl Recognizer<'_, '_> {
    /// `fs_routes` rows active for this file (installed activating package, matching glob).
    pub(super) fn fs_route_rows(&self, rec: &FileRecord) -> Vec<trace_library::table::IrreducibleRow> {
        self.input
            .tables
            .irreducible(rec.language, Section::FsRoutes)
            .iter()
            .filter(|r| r.active(&|p| self.installed(rec.language, p)))
            .filter(|r| {
                r.glob
                    .as_deref()
                    .is_some_and(|g| trace_syntax::testing::glob_matches_within(g, &rec.path).is_some())
            })
            .cloned()
            .collect()
    }

    pub(super) fn fs_route(
        &self,
        site: &Site<'_>,
        row: &trace_library::table::IrreducibleRow,
        out: &mut Vec<BoundaryFact>,
    ) {
        let Some(glob) = row.glob.as_deref() else { return };
        let Some(base) = trace_syntax::testing::glob_matches_within(glob, &site.rec.path) else { return };
        let rel = match base.is_empty() {
            true => site.rec.path.clone(),
            false => site.rec.path[base.len() + 1..].to_string(),
        };
        let Some(key) = fs_route_key(glob, row.root.as_deref(), &rel) else { return };
        let Some(norm) = normalize(&Tpl::literal(&key), false, &self.placeholders) else { return };
        let exports = site.parsed.exports();
        let handler = row.handler_sel().ok().flatten();
        let verb = row.verb_sel().ok().flatten();
        let mut targets: Vec<(String, &trace_syntax::boundary::Export)> = Vec::new();
        match (&handler, &row.pattern) {
            // Rows naming the exported function (`loader`, `action`).
            (_, Some(pattern)) => {
                for e in exports.iter().filter(|e| &e.name == pattern) {
                    targets.push((row_verb(&verb, None), e));
                }
            }
            (Some(RowSel::DefaultExport), None) => {
                for e in exports.iter().filter(|e| e.name == "default") {
                    targets.push((row_verb(&verb, None), e));
                }
            }
            (Some(RowSel::ExportedNames), None) => {
                for e in &exports {
                    if let Some(v) = verb_of(&e.name).filter(|v| *v == e.name) {
                        targets.push((row_verb(&verb, Some(&v)), e));
                    }
                }
            }
            _ => {}
        }
        for (method, e) in targets {
            let decl = e
                .name_span
                .and_then(|s| decl_at(site.facts, s))
                .or_else(|| decl_at(site.facts, e.span));
            let mut detail = common_detail(&format!("filesystem route {glob}"), false);
            detail.push((
                "framework".into(),
                format!("filesystem route ({})", row.activated_by.as_deref().unwrap_or(glob)),
            ));
            detail.push(("method".into(), method.clone()));
            detail.push(("path".into(), norm.path.clone()));
            detail.push(("handler_span".into(), format!("{}:{}", e.span.start, e.span.end)));
            out.push(fact(
                site,
                BridgeKind::Http,
                BoundaryRole::Provides,
                format!("{method} {}", norm.path),
                None,
                decl,
                e.span,
                detail,
            ));
        }
    }
}

/// Route key of a file under a filesystem-routing glob: the path below the row's URL `root`
/// (the glob's static directory prefix when the row names none), without the extension; a
/// static file name in the glob (`route.ts`, `+server.ts`) and `index` files name their
/// directory.
fn fs_route_key(glob: &str, url_root: Option<&str>, rel: &str) -> Option<String> {
    let gsegs: Vec<&str> = glob.split('/').filter(|s| !s.is_empty()).collect();
    let is_static = |s: &str| !s.contains(['*', '?', '[', '{']);
    let root: Vec<&str> = match url_root {
        Some(r) => r.split('/').filter(|s| !s.is_empty()).collect(),
        None => gsegs.iter().take_while(|s| is_static(s)).copied().collect(),
    };
    let rsegs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if rsegs.len() < root.len() || rsegs[..root.len()] != root[..] {
        return None;
    }
    let mut rest: Vec<String> = rsegs[root.len()..].iter().map(|s| s.to_string()).collect();
    let file = rest.pop()?;
    let stem = file.split('.').next().unwrap_or(&file).to_string();
    let static_file = gsegs.last().is_some_and(|g| {
        let gstem = g.split('.').next().unwrap_or(g);
        is_static(gstem)
    });
    if !static_file && stem != "index" {
        rest.push(stem);
    }
    Some(format!("/{}", rest.join("/")))
}

/// The verb of an fs-route row (`exported` = the exported function's verb name).
fn row_verb(verb: &Option<RowVerb>, exported: Option<&str>) -> String {
    match verb {
        Some(RowVerb::ExportedNames) => exported.map(str::to_string).unwrap_or_else(|| "*".into()),
        Some(RowVerb::Sel(VerbSel::Const(v))) => verb_of(v).unwrap_or_else(|| "*".into()),
        _ => exported.map(str::to_string).unwrap_or_else(|| "*".into()),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/recognize/fs_routes.rs"]
mod tests;
