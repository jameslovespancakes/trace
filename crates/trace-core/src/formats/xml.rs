//! A small element tree over `quick-xml` (pom.xml, *.csproj, Directory.Build.props,
//! .classpath, nuget.config). Text is trimmed and unescaped; comments, processing
//! instructions and the doctype are dropped.

use std::collections::BTreeMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Maximum element depth read (deeper documents are rejected).
const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    /// Qualified name as written (`project`, `x:item`).
    pub name: String,
    pub attributes: BTreeMap<String, String>,
    pub children: Vec<Element>,
    /// Concatenated text and CDATA directly inside this element (trimmed).
    pub text: String,
}

impl Element {
    /// Name without a namespace prefix.
    pub fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// First child with this local name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.local_name() == name)
    }

    /// All children with this local name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.local_name() == name)
    }

    /// Text of the element reached by following child local names (`["parent", "version"]`).
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        let mut e = self;
        for name in path {
            e = e.child(name)?;
        }
        Some(e.text.as_str())
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }
}

/// Parse a document into its root element. `None` for malformed documents.
pub fn parse(text: &str) -> Option<Element> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    return None;
                }
                stack.push(element(&start)?);
            }
            Event::Empty(start) => {
                let e = element(&start)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(e),
                    None => root = root.or(Some(e)),
                }
            }
            Event::End(_) => {
                let e = stack.pop()?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(e),
                    None => root = root.or(Some(e)),
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(t.unescape().ok()?.trim());
                }
            }
            Event::CData(c) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(std::str::from_utf8(&c.into_inner()).ok()?.trim());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return None;
    }
    root
}

fn element(start: &BytesStart<'_>) -> Option<Element> {
    let name = std::str::from_utf8(start.name().as_ref()).ok()?.to_string();
    let mut attributes = BTreeMap::new();
    for attr in start.attributes() {
        let attr = attr.ok()?;
        let key = std::str::from_utf8(attr.key.as_ref()).ok()?.to_string();
        let value = attr.unescape_value().ok()?.into_owned();
        attributes.insert(key, value);
    }
    Some(Element {
        name,
        attributes,
        children: Vec::new(),
        text: String::new(),
    })
}

#[cfg(test)]
#[path = "../../tests/unit/formats/xml.rs"]
mod tests;
