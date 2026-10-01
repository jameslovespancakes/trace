//! Owned syntax arena: tree-sitter nodes copied into indices (shared by the R and JVM
//! ecosystems), so callers walk a manifest's syntax tree without borrowing tree-sitter types.

use trace_core::Language;

/// Bound of the syntax arena (nodes) for one manifest file.
const MAX_NODES: usize = 400_000;

/// A syntax tree copied into an index arena (pre-order: parents before children), so callers
/// walk it without borrowing tree-sitter types.
pub(crate) struct Syn<'s> {
    src: &'s [u8],
    nodes: Vec<SynNode>,
}

struct SynNode {
    kind: &'static str,
    field: Option<&'static str>,
    start: usize,
    end: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    named: bool,
}

impl<'s> Syn<'s> {
    /// Parse `src` with the grammar of `language` (None when there is no grammar or the
    /// parser fails). At most [`MAX_NODES`] nodes are kept.
    pub fn parse(language: Language, src: &'s [u8]) -> Option<Syn<'s>> {
        let tree = trace_syntax::parse_tree(language, src).ok()?;
        let mut cursor = tree.walk();
        let mut nodes: Vec<SynNode> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        loop {
            let node = cursor.node();
            let index = nodes.len();
            let parent = stack.last().copied();
            nodes.push(SynNode {
                kind: node.kind(),
                field: cursor.field_name(),
                start: node.start_byte(),
                end: node.end_byte(),
                parent,
                children: Vec::new(),
                named: node.is_named(),
            });
            if let Some(p) = parent {
                nodes[p].children.push(index);
            }
            if nodes.len() >= MAX_NODES {
                break;
            }
            if cursor.goto_first_child() {
                stack.push(index);
                continue;
            }
            let mut finished = false;
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    finished = true;
                    break;
                }
                stack.pop();
            }
            if finished {
                break;
            }
        }
        Some(Syn { src, nodes })
    }

    /// Number of nodes (index 0 is the root).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn kind(&self, i: usize) -> &'static str {
        self.nodes[i].kind
    }

    /// Source text of the node ("" when it is not UTF-8).
    pub fn text(&self, i: usize) -> &'s str {
        let n = &self.nodes[i];
        self.src
            .get(n.start..n.end)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    pub fn parent(&self, i: usize) -> Option<usize> {
        self.nodes[i].parent
    }

    /// Named children in source order.
    pub fn named_children(&self, i: usize) -> Vec<usize> {
        self.nodes[i]
            .children
            .iter()
            .copied()
            .filter(|c| self.nodes[*c].named)
            .collect()
    }

    /// The child stored under the grammar field `name`.
    pub fn field(&self, i: usize, name: &str) -> Option<usize> {
        self.nodes[i]
            .children
            .iter()
            .copied()
            .find(|c| self.nodes[*c].field == Some(name))
    }

    /// Preorder descendants of `i` (including `i`).
    pub fn subtree(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in self.nodes[n].children.iter().rev() {
                stack.push(*c);
            }
        }
        out
    }

    /// Value of a string literal (`"x"`, `'x'`) or the text of an identifier. None for
    /// interpolated or escaped strings.
    pub fn literal(&self, i: usize) -> Option<String> {
        match self.kind(i) {
            "string" => {
                let mut out = String::new();
                for c in self.named_children(i) {
                    match self.kind(c) {
                        "string_content" => out.push_str(self.text(c)),
                        // R strings name their delimiters as nodes.
                        "string_open" | "string_close" => {}
                        _ => return None,
                    }
                }
                Some(out)
            }
            "identifier" => Some(self.text(i).to_string()),
            _ => None,
        }
    }
}
