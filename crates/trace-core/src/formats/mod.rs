//! Shared structured readers for configuration and manifest formats (never regex).
//!
//! * [`yaml`]  — block-YAML subset reader (OpenAPI documents, CI files).
//! * [`ini`]   — `KEY=VALUE` files and INI sections (pyvenv.cfg, release, setup.cfg).
//! * [`jsonc`] — JSON with comments and trailing commas (tsconfig, devcontainer).
//! * [`xml`]   — element tree over `quick-xml` (pom.xml, *.csproj, .classpath).
//! * [`toml`]  — the `toml` crate (Cargo.toml, pyproject.toml, uv.lock); [`toml_value`]
//!   converts a document into a `serde_json::Value`.

pub mod ini;
pub mod jsonc;
pub mod xml;
pub mod yaml;

pub use toml;

/// Parse a TOML document into JSON values (datetimes become strings). `None` when the
/// document does not parse.
pub fn toml_value(text: &str) -> Option<serde_json::Value> {
    let value: toml::Value = toml::from_str(text).ok()?;
    serde_json::to_value(value).ok()
}

#[cfg(test)]
#[path = "../../tests/unit/formats/mod.rs"]
mod tests;
