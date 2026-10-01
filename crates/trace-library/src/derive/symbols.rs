//! Library-qualified symbols and declaration positions of derived functions and classes
//! (child of `derive`).

use super::*;

impl<'c> Program<'c> {
    pub(super) fn symbol_of(&self, u: u32, qualified: &str) -> String {
        let sep = self.spec.symbol_separator;
        let q = if sep == "." {
            qualified.to_string()
        } else {
            qualified.replace('.', sep)
        };
        match &self.units[u as usize].module {
            Some(m) => format!("{m}{sep}{q}"),
            None => q,
        }
    }

    pub(super) fn func_symbol(&self, f: u32) -> String {
        let func = &self.funcs[f as usize];
        self.symbol_of(func.unit, &func.qualified)
    }

    pub(super) fn class_symbol(&self, c: u32) -> String {
        let class = &self.classes[c as usize];
        self.symbol_of(class.unit, &class.qualified)
    }

    /// Byte position of a declaration name: (0-based line, byte column).
    pub(super) fn position(&self, u: u32, decl: u32) -> (u32, u32) {
        let unit = &self.units[u as usize];
        let file = &unit.file;
        let start = file.facts.declarations[decl as usize].name_span.start;
        let line = file.lines.line0(start);
        let line_start = file.lines.line_span(&file.source, line).map(|s| s.start).unwrap_or(0);
        (line, start.saturating_sub(line_start))
    }
}
