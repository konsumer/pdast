# Agent Instructions

This file provides essential context for AI agents working with the pdast codebase.

## What This Project Is

pdast (PureData AST) is a Rust workspace that parses PureData `.pd` patch files into a JSON-serializable AST, and can serialize the AST back to `.pd`. It targets multiple platforms: native Rust, WebAssembly (JS hosts via wasm-bindgen and non-JS WASM), and downstream code generators.

**Core Purpose**: Enable code generation from PureData patches by first converting them to a structured AST that can be transformed into various output formats (Faust DSP, Teensy C++, etc.).

## Project Structure

Rust workspace with 4 crates:

```
pdast/        - Core library (parse .pd → AST, emit AST → .pd, JSON serialization)
pd2ast/       - CLI: load patch from disk, resolve abstractions, print JSON
ast2pd/       - CLI: convert JSON AST back to .pd patch
pdast2faust/  - CLI: read JSON AST and generate Faust DSP code
```

Additional files:
- `pdast.py` - Python wrapper that calls CLI tools
- `web/` - Web components and demo pages
- `node/` - Node.js package wrapper
- `tests/` - Test fixtures and integration tests

## Build Commands

```bash
# Build all crates
cargo build --release

# Build WASM for JS/browser
wasm-pack build --target web pdast --features wasm-js

# Build WASM for non-JS hosts
cargo build -p pdast --target wasm32-wasip1 --release

# Run all tests
cargo test --workspace

# Format code
cargo fmt

# Install CLI tools locally
cargo install --path pd2ast
cargo install --path ast2pd
cargo install --path pdast2faust
```

## NPM Scripts (for JS/WASM)

```bash
npm run build      # Build WASM package
npm run start      # Start dev server (builds first)
npm run format     # Format with prettier
```

## PureData File Format (Key Facts)

- Plain text, record-based, CRLF line endings
- `#N` = new canvas (root or sub-patch)
- `#X` = objects, messages, connections, restore, coords, etc.
- `#A` = array data
- Object indices are implicit (0-based, sequential, skip `connect`/`restore`)
- Colors stored as negative integers: `-(R*65536 + G*256 + B)`
- Message content uses `\,` and `\;` escapes

## AST Design Summary

**Token Types**: Float(f64), Symbol(String), Dollar(u32), DollarZero

**Node Kinds**: Obj, Msg, FloatAtom, SymbolAtom, Text, SubPatch, Graph, Gui, Array, Unknown

**Key Types**:
- `Canvas` - Contains nodes, connections, sub-patches
- `Connection` - src_node, src_outlet, dst_node, dst_inlet
- `Patch` - Root canvas
- `ParseResult` - Patch + warnings

## Abstraction Resolution

Parser accepts a loader callback: `Fn(&str) -> Option<String>`
- Called when object name could be an abstraction
- Returns patch content if found, None if unknown
- Works without filesystem (WASM-friendly)

## WASM Strategy

Two interfaces:
1. **JS hosts**: `wasm_bindgen` functions return JsValue via serde-wasm-bindgen
2. **Non-JS hosts**: C ABI functions (`wasm_alloc`, `wasm_dealloc`, `wasm_parse_to_json_abi`, etc.) with JSON string interchange

## Code Generation Target: Faust

Maps PD objects to Faust library functions:
- `osc~` → `os.osc`
- `lop~` → `fi.lowpass(1, freq)`
- `*~` → `*`, `+~` → `+`, etc.
- GUI objects → Faust UI primitives
- Objects without templates emit `_` (passthrough) with warning

Template files (`.dsp`) define `pdobj` with optional parameters matching PD creation args.

## Common Agent Tasks

**Adding new PD object support:**
1. Update parser if needed (vanilla object parsing in `pdast/src/parse/obj.rs`)
2. Add to token types if new syntax required
3. Update emitter if needed (`pdast/src/emit/`)
4. Add Faust template to `pdast2faust/src/lib/templates/` or lib directory

**Extending WASM interface:**
1. Add function to `pdast/src/wasm.rs`
2. Use `#[wasm_bindgen]` for JS, plain functions with C ABI for non-JS
3. Follow existing pattern: allocate → call → read result → dealloc

**Adding CLI tool:**
1. Create new crate with `[[bin]]` section
2. Use `clap` for argument parsing (follow existing pattern)
3. Import `pdast` crate for core functionality
4. Add to workspace `Cargo.toml`

**Fixing parsing issues:**
1. Check tokenizer in `pdast/src/parse/mod.rs`
2. Verify object dispatch in `pdast/src/parse/obj.rs`
3. Check GUI parsing in `pdast/src/parse/gui.rs`
4. Add test case to `tests/`

## Important Notes

- **No structural validation during parsing** - warnings collected separately
- **Line endings**: Always emit CRLF (`\r\n`) regardless of input
- **Object indices**: Critical for connections - must be regenerated correctly in emitter
- **Unknown objects**: Stored as `NodeKind::Unknown` with preserved connections
- **Sub-patches**: Inline content embedded, abstractions resolved via loader
- **WASM memory**: Non-JS hosts must manually allocate/dealloc strings

## Dependencies to Know

Core: `serde`, `serde_json`, `thiserror`
WASM: `wasm-bindgen`, `serde-wasm-bindgen`, `js-sys` (optional)
CLI: `clap`

## Key References

- PD format: https://puredata.info/docs/developer/PdFileFormat
- Faust: https://faust.grame.fr/doc/manual
- HVCC (similar project): https://github.com/Wasted-Audio/hvcc
- Teensy Audio: https://pjrc.com/teensy/td_libs_Audio.html
