//! # pdast
//!
//! A Rust library for parsing PureData `.pd` patch files into a JSON-serializable
//! AST, and emitting them back.
//!
//! ## Basic usage
//!
//! ```rust
//! use pdast::{parse_patch_no_loader, emit_patch, to_json};
//!
//! let pd_source = "#N canvas 0 50 450 300 12;\r\n\
//!                  #X obj 30 27 osc~ 440;\r\n\
//!                  #X obj 30 60 dac~;\r\n\
//!                  #X connect 0 0 1 0;\r\n\
//!                  #X connect 0 0 1 1;\r\n";
//!
//! let result = parse_patch_no_loader(pd_source).unwrap();
//! println!("{} nodes", result.patch.root.nodes.len());
//!
//! let json = to_json(&result.patch).unwrap();
//! let pd_out = emit_patch(&result.patch);
//! ```

pub mod emit;
pub mod error;
pub mod parse;
pub mod types;

pub use emit::emit_patch;
pub use error::ParseError;
pub use parse::{parse_patch, parse_patch_no_loader};
pub use types::*;

/// Serialize a `Patch` to a JSON string.
pub fn to_json(patch: &Patch) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(patch)
}

/// Deserialize a `Patch` from a JSON string.
pub fn from_json(json: &str) -> Result<Patch, serde_json::Error> {
    serde_json::from_str(json)
}

/// Serialize a full `ParseResult` (patch + warnings) to a JSON string.
pub fn result_to_json(result: &ParseResult) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(result)
}
