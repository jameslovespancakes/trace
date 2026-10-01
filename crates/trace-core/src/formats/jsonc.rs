//! JSON with comments (`//`, `/* */`) and trailing commas, as written in `tsconfig.json`,
//! `.vscode/settings.json`: a character-level scanner that removes comments
//! and trailing commas outside strings, then the result is read by `serde_json`.

use serde_json::Value;

/// Parse a JSONC document. `None` when it is not valid JSON after comment/comma removal.
pub fn parse(text: &str) -> Option<Value> {
    serde_json::from_str(&strip(text)).ok()
}

/// Remove comments and trailing commas outside string literals. Byte offsets are not kept;
/// newlines inside removed block comments are kept so serde_json errors keep their line.
pub fn strip(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 2;
                    continue;
                }
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    if chars[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
                i = (i + 2).min(chars.len());
            }
            ',' => {
                // Trailing comma: the next significant character closes the container.
                let mut j = i + 1;
                loop {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                        while j < chars.len() && chars[j] != '\n' {
                            j += 1;
                        }
                        continue;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                        j += 2;
                        while j < chars.len() && !(chars[j] == '*' && chars.get(j + 1) == Some(&'/')) {
                            j += 1;
                        }
                        j = (j + 2).min(chars.len());
                        continue;
                    }
                    break;
                }
                if !matches!(chars.get(j), Some('}') | Some(']')) {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "../../tests/unit/formats/jsonc.rs"]
mod tests;
