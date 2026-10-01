pub use crate::util::Replacer;

pub mod standard;
mod util;

// Feature-gated modules: rust-analyzer only sees them when the `serde` feature is enabled.
#[cfg(feature = "serde")]
mod json;
#[cfg(feature = "serde")]
mod jsont;

pub fn run_json() {
    #[cfg(feature = "serde")]
    json::JSONSink::default().replace(b"text");
}
