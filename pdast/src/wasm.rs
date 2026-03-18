//! WASM bindings for pdast.
//!
//! Two layers are provided:
//!
//! ## 1. JSON string API (always compiled for `target_arch = "wasm32"`)
//!
//! These functions use only primitive types (`&str` / `String`) so they work
//! in any WASM host: browsers, Node.js, WASI, component-model runtimes, etc.
//! No `wasm-bindgen` required.
//!
//! | Function | Input | Output |
//! |---|---|---|
//! | [`wasm_parse_to_json`] | patch text + abstractions JSON | JSON AST |
//! | [`wasm_emit_to_pd`] | JSON AST | `.pd` text |
//! | [`wasm_patch_to_pd`] | patch text + abstractions JSON | `.pd` text (roundtrip) |
//!
//! ### Abstractions map
//!
//! Because WASM functions cannot easily accept callbacks, abstractions are
//! supplied as a JSON object mapping abstraction name to patch content:
//!
//! ```json
//! { "my-filter": "#N canvas 0 0 400 300 12;\n..." }
//! ```
//!
//! Pass `"{}"` or `"null"` when no abstractions are needed.
//!
//! ### Error handling
//!
//! On error, functions return a JSON object `{"error": "message"}`.
//! On success, functions return the expected value directly.
//!
//! ## 2. JS-host API (compiled only with `--features wasm-js`)
//!
//! Requires `wasm-bindgen`. Exports `#[wasm_bindgen]` functions that accept
//! and return native JS types.
//!
//! | Function | Notes |
//! |---|---|
//! | [`js::js_parse`] | Returns a JS object; accepts an optional JS loader callback |
//! | [`js::js_parse_to_json`] | Same but returns a JSON string |
//! | [`js::js_emit`] | Accepts a JS object AST, returns `.pd` string |
//!
//! Build with `wasm-pack`:
//! ```sh
//! wasm-pack build pdast --features wasm-js
//! ```
//! Or with cargo directly:
//! ```sh
//! cargo build -p pdast --target wasm32-unknown-unknown --features wasm-js
//! ```

use std::collections::HashMap;

use crate::{emit_patch, from_json, parse_patch, result_to_json};

// ── Shared helper ─────────────────────────────────────────────────────────────

/// Parse an abstractions JSON string into a name→content map.
/// Accepts `"{}"`, `"null"`, empty string, or `{"name": "content", ...}`.
fn make_loader_map(abstractions_json: &str) -> HashMap<String, String> {
    if abstractions_json.is_empty() || abstractions_json == "null" || abstractions_json == "{}" {
        return HashMap::new();
    }
    serde_json::from_str::<HashMap<String, String>>(abstractions_json).unwrap_or_default()
}

fn error_json(msg: &str) -> String {
    format!(
        "{{\"error\":{}}}",
        serde_json::to_string(msg).unwrap_or_else(|_| "\"unknown\"".into())
    )
}

/// Attempt to deserialise `json` as a bare `Patch`, falling back to unwrapping
/// a `ParseResult` envelope if that fails.
fn patch_from_json_flexible(json: &str) -> Result<crate::Patch, String> {
    from_json(json).or_else(|_| {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let patch_val = v
            .get("patch")
            .cloned()
            .ok_or_else(|| "not a Patch or ParseResult".to_string())?;
        serde_json::from_value(patch_val).map_err(|e| e.to_string())
    })
}

// ── JSON string API ───────────────────────────────────────────────────────────

/// Parse a PureData patch and return a JSON `ParseResult` string.
///
/// `abstractions_json` is a JSON object mapping abstraction name → patch
/// content, e.g. `{"my-filter": "#N canvas ..."}`. Pass `"{}"` or `"null"`
/// when no abstractions are needed.
///
/// Returns a JSON object. On success: `{"patch": {...}, "warnings": [...]}`.
/// On error: `{"error": "..."}`.
pub fn wasm_parse_to_json(patch_content: &str, abstractions_json: &str) -> String {
    let map = make_loader_map(abstractions_json);
    match parse_patch(patch_content, |name| map.get(name).cloned()) {
        Ok(result) => result_to_json(&result).unwrap_or_else(|e| error_json(&e.to_string())),
        Err(e) => error_json(&e.to_string()),
    }
}

/// Emit a `.pd` patch from a JSON AST string.
///
/// Accepts the JSON produced by [`wasm_parse_to_json`] (either the full
/// `ParseResult` object with a `"patch"` key, or a bare `Patch` object).
///
/// Returns the `.pd` text on success, or `{"error": "..."}` on failure.
pub fn wasm_emit_to_pd(ast_json: &str) -> String {
    match patch_from_json_flexible(ast_json) {
        Ok(p) => emit_patch(&p),
        Err(e) => error_json(&e),
    }
}

/// Parse a PureData patch and immediately emit it back as a `.pd` string
/// (a parse → emit roundtrip in one call).
///
/// Useful for normalising a patch or verifying roundtrip fidelity without
/// needing to deserialise the AST on the host side.
pub fn wasm_patch_to_pd(patch_content: &str, abstractions_json: &str) -> String {
    let map = make_loader_map(abstractions_json);
    match parse_patch(patch_content, |name| map.get(name).cloned()) {
        Ok(result) => emit_patch(&result.patch),
        Err(e) => error_json(&e.to_string()),
    }
}

// ── `extern "C"` ABI for non-bindgen WASM hosts ───────────────────────────────
//
// These give WASI runtimes and other non-JS hosts a stable C ABI.
// Convention:
//   1. Host allocates input strings in WASM memory with `wasm_alloc`.
//   2. Host calls a function with (ptr, len) pairs.
//   3. Function allocates result memory and returns `(ptr << 32) | len`.
//   4. Host reads the result bytes, then frees with `wasm_dealloc(ptr, len)`.

/// Allocate `size` bytes in WASM linear memory. The caller must free the
/// returned pointer with [`wasm_dealloc`] when done.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_alloc(size: u32) -> *mut u8 {
    let mut buf: Vec<u8> = Vec::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free memory previously returned by [`wasm_alloc`] or an ABI function.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_dealloc(ptr: *mut u8, size: u32) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: ptr+size were allocated by wasm_alloc with Vec<u8>.
    unsafe {
        drop(Vec::from_raw_parts(ptr, size as usize, size as usize));
    }
}

/// Write `s` into a new WASM allocation and return `(ptr << 32) | len`.
fn string_to_abi(s: String) -> i64 {
    let bytes = s.into_bytes();
    let len = bytes.len() as u32;
    let ptr = wasm_alloc(len);
    // SAFETY: ptr is freshly allocated with exactly `len` bytes capacity.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len as usize);
    }
    ((ptr as i64) << 32) | (len as i64)
}

/// Read a `&str` from a (ptr, len) pair pointing into WASM linear memory.
///
/// # Safety
/// `ptr` must point to `len` bytes of valid UTF-8 within WASM memory that
/// remain valid for the duration of the call.
unsafe fn str_from_parts(ptr: *const u8, len: u32) -> &'static str {
    // SAFETY: upheld by caller contract.
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    std::str::from_utf8(slice).unwrap_or("")
}

/// Parse a patch and return a JSON AST.
///
/// - `patch_ptr/patch_len`: UTF-8 patch text in WASM memory.
/// - `abs_ptr/abs_len`: UTF-8 abstractions JSON in WASM memory.
///
/// Returns `(result_ptr << 32) | result_len`. Free result with `wasm_dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_parse_to_json_abi(
    patch_ptr: *const u8,
    patch_len: u32,
    abs_ptr: *const u8,
    abs_len: u32,
) -> i64 {
    // SAFETY: caller guarantees valid UTF-8 in WASM memory.
    let patch = unsafe { str_from_parts(patch_ptr, patch_len) };
    let abs = unsafe { str_from_parts(abs_ptr, abs_len) };
    string_to_abi(wasm_parse_to_json(patch, abs))
}

/// Emit a `.pd` patch from a JSON AST.
///
/// - `ast_ptr/ast_len`: UTF-8 JSON AST string in WASM memory.
///
/// Returns `(result_ptr << 32) | result_len`. Free result with `wasm_dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_emit_to_pd_abi(ast_ptr: *const u8, ast_len: u32) -> i64 {
    // SAFETY: caller guarantees valid UTF-8 in WASM memory.
    let ast = unsafe { str_from_parts(ast_ptr, ast_len) };
    string_to_abi(wasm_emit_to_pd(ast))
}

/// Parse a patch and immediately emit it back as `.pd` text.
///
/// - `patch_ptr/patch_len`: UTF-8 patch text in WASM memory.
/// - `abs_ptr/abs_len`: UTF-8 abstractions JSON in WASM memory.
///
/// Returns `(result_ptr << 32) | result_len`. Free result with `wasm_dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_patch_to_pd_abi(
    patch_ptr: *const u8,
    patch_len: u32,
    abs_ptr: *const u8,
    abs_len: u32,
) -> i64 {
    // SAFETY: caller guarantees valid UTF-8 in WASM memory.
    let patch = unsafe { str_from_parts(patch_ptr, patch_len) };
    let abs = unsafe { str_from_parts(abs_ptr, abs_len) };
    string_to_abi(wasm_patch_to_pd(patch, abs))
}

// ── JS-host API (wasm-bindgen) ────────────────────────────────────────────────

#[cfg(feature = "wasm-js")]
mod js {
    use super::*;
    use js_sys::Function;
    use wasm_bindgen::prelude::*;

    /// Parse a PureData patch and return the AST as a JS object.
    ///
    /// `patch_content` — the `.pd` file text.
    ///
    /// `loader` — an optional JS function `(name: string) => string | null`
    /// called to resolve abstraction bodies. Return the patch content string,
    /// or `null`/`undefined` if unavailable.
    ///
    /// Returns a JS object matching the `ParseResult` schema
    /// (`{ patch: {...}, warnings: [...] }`).
    ///
    /// Throws a JS `Error` if the patch cannot be parsed at all.
    #[wasm_bindgen(js_name = "parse")]
    pub fn js_parse(patch_content: &str, loader: Option<Function>) -> Result<JsValue, JsValue> {
        let result = parse_patch(patch_content, |name| {
            let f = loader.as_ref()?;
            let ret = f.call1(&JsValue::NULL, &JsValue::from_str(name)).ok()?;
            if ret.is_null() || ret.is_undefined() {
                None
            } else {
                ret.as_string()
            }
        })
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

        serde_wasm_bindgen::to_value(&result).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Parse a PureData patch and return the AST as a JSON string.
    ///
    /// Same as [`js_parse`] but returns a JSON string instead of a JS object.
    /// Throws a JS `Error` on parse failure.
    #[wasm_bindgen(js_name = "parseToJson")]
    pub fn js_parse_to_json(
        patch_content: &str,
        loader: Option<Function>,
    ) -> Result<String, JsValue> {
        let result = parse_patch(patch_content, |name| {
            let f = loader.as_ref()?;
            let ret = f.call1(&JsValue::NULL, &JsValue::from_str(name)).ok()?;
            if ret.is_null() || ret.is_undefined() {
                None
            } else {
                ret.as_string()
            }
        })
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

        result_to_json(&result).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Emit a `.pd` patch string from a JS AST object.
    ///
    /// Accepts the JS object returned by [`js_parse`], or an object
    /// deserialised from the JSON returned by [`js_parse_to_json`].
    /// Also accepts a bare `Patch` object (without the `warnings` wrapper).
    ///
    /// Throws a JS `Error` if the object cannot be deserialised.
    #[wasm_bindgen(js_name = "emitPatch")]
    pub fn js_emit(ast: JsValue) -> Result<String, JsValue> {
        // Accept either a bare Patch or a ParseResult wrapper
        let patch = serde_wasm_bindgen::from_value::<crate::Patch>(ast.clone()).or_else(|_| {
            serde_wasm_bindgen::from_value::<crate::ParseResult>(ast).map(|r| r.patch)
        });
        let patch = patch.map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(emit_patch(&patch))
    }

    /// Emit a `.pd` patch string from a JSON AST string.
    ///
    /// Convenience wrapper for when you have a JSON string rather than a JS
    /// object. Throws a JS `Error` on failure.
    #[wasm_bindgen(js_name = "emitPatchFromJson")]
    pub fn js_emit_from_json(ast_json: &str) -> Result<String, JsValue> {
        let patch = patch_from_json_flexible(ast_json).map_err(|e| JsValue::from_str(&e))?;
        Ok(emit_patch(&patch))
    }
}
