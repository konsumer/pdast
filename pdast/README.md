# pdast

Core library — parse PureData `.pd` patches to a JSON-serializable AST, and emit AST back to `.pd`. Used as a Rust crate, or compiled to WASM for JS/browser or non-JS hosts.

## Using pdast as a WASM / JavaScript package

### Build

```sh
# JS/browser/Node (wasm-bindgen, full JS API)
wasm-pack build pdast --features wasm-js
# Output: pdast/pkg/  — an npm-ready package

# Plain WASM (WASI, component model, any non-JS host)
cargo build -p pdast --target wasm32-wasip1 --release
```

### JavaScript / TypeScript (wasm-pack output)

```js
import { parse, parseToJson, emitPatch, emitPatchFromJson } from './pdast/pkg/pdast.js'

const pd = `#N canvas 0 50 450 300 12;\r\n#X obj 30 27 osc~ 440;\r\n...`

// Parse to a JS object ({ patch: {...}, warnings: [...] })
const result = parse(pd)
console.log(result.patch.root.nodes)

// Parse with an abstraction loader callback
const result2 = parse(pd, (name) => {
  // return the .pd file content for `name`, or null if unavailable
  return fetch(`/patches/${name}.pd`).then((r) => r.text()) // async also works
})

// Emit a JS object back to .pd text
const pdOut = emitPatch(result)

// Parse → JSON string (useful for storage or passing to another language)
const json = parseToJson(pd)
const pdOut2 = emitPatchFromJson(json)
```

All four exported functions throw a JS `Error` on failure.

### Non-JS WASM hosts (WASI / raw ABI)

The module always exports these low-level C ABI functions, usable from any WASM runtime:

| Export                                                                  | Description                     |
| ----------------------------------------------------------------------- | ------------------------------- |
| `wasm_alloc(size: i32) -> i32`                                          | Allocate bytes in WASM memory   |
| `wasm_dealloc(ptr: i32, size: i32)`                                     | Free previously allocated bytes |
| `wasm_parse_to_json_abi(patch_ptr, patch_len, abs_ptr, abs_len) -> i64` | Parse patch → JSON AST          |
| `wasm_emit_to_pd_abi(ast_ptr, ast_len) -> i64`                          | JSON AST → `.pd` text           |
| `wasm_patch_to_pd_abi(patch_ptr, patch_len, abs_ptr, abs_len) -> i64`   | Parse + emit in one call        |

All string functions follow the same convention:

1. Allocate input strings in WASM memory with `wasm_alloc`.
2. Call the function with `(ptr: i32, len: i32)` pairs.
3. The return value encodes the result as `(ptr << 32) | len` in a single `i64`.
4. Read the result bytes from WASM memory, then free with `wasm_dealloc(ptr, len)`.

The `abs_ptr/abs_len` parameter for parse functions is a JSON object string mapping abstraction names to patch content: `{"my-filter": "#N canvas ..."}`. Pass an empty string or `"{}"` for no abstractions.

## Using pdast as a Rust library

Add to your `Cargo.toml`:

```toml
[dependencies]
pdast = { path = "../pdast" }   # or publish to crates.io
```

### Parse a patch

```rust
use pdast::{parse_patch, parse_patch_no_loader};

// Without abstraction resolution
let result = parse_patch_no_loader(pd_source).unwrap();
println!("{} nodes", result.patch.root.nodes.len());
for w in &result.warnings { eprintln!("warning: {}", w.message); }

// With a filesystem loader
let result = parse_patch(pd_source, |name| {
    std::fs::read_to_string(format!("{}.pd", name)).ok()
}).unwrap();
```

### Inspect the AST

```rust
use pdast::types::{NodeKind, SubPatchContent, Token};

for node in &result.patch.root.nodes {
    match &node.kind {
        NodeKind::Obj { name, args } => println!("obj: {name}"),
        NodeKind::Gui(g) => println!("gui: {:?}", g.kind),
        NodeKind::SubPatch { name, content, .. } => {
            if let SubPatchContent::Inline(canvas) = content {
                println!("subpatch {name}: {} nodes", canvas.nodes.len());
            }
        }
        NodeKind::Text { content } => println!("// {content}"),
        _ => {}
    }
}
```

### Emit back to .pd

```rust
use pdast::emit_patch;

let pd_text = emit_patch(&result.patch);
std::fs::write("output.pd", pd_text).unwrap();
```

### JSON roundtrip

```rust
use pdast::{to_json, from_json};

let json = to_json(&result.patch).unwrap();
let patch = from_json(&json).unwrap();
```

## JSON AST shape

A minimal patch with one object:

```json
{
  "root": {
    "x": 0,
    "y": 50,
    "width": 450,
    "height": 300,
    "font_size": 12,
    "name": null,
    "open_on_load": false,
    "coords": null,
    "nodes": [
      {
        "id": 0,
        "x": 30,
        "y": 27,
        "kind": {
          "kind": "obj",
          "name": "osc~",
          "args": [{ "type": "Float", "value": 440.0 }]
        }
      }
    ],
    "connections": [{ "src_node": 0, "src_outlet": 0, "dst_node": 1, "dst_inlet": 0 }]
  }
}
```

### Node kind values

| `kind` field    | Description                                                              |
| --------------- | ------------------------------------------------------------------------ |
| `"obj"`         | Object box (vanilla or external) — has `name` and `args`                 |
| `"msg"`         | Message box — has `messages` (array of arrays of tokens)                 |
| `"float_atom"`  | Number box (`floatatom`)                                                 |
| `"symbol_atom"` | Symbol box (`symbolatom`)                                                |
| `"text"`        | Comment — has `content`                                                  |
| `"sub_patch"`   | Inline sub-patch or resolved abstraction — has `name`, `args`, `content` |
| `"graph"`       | Graph canvas (for arrays) — has `content`                                |
| `"gui"`         | IEM GUI object — has `gui_kind`, `width`, `height`, `min`, `max`, etc.   |
| `"array"`       | Sample array — has `name`, `size`, `flags`, `data`                       |
| `"unknown"`     | Unresolved external or broken box                                        |

### Token values

```json
{ "type": "Float",      "value": 440.0 }
{ "type": "Symbol",     "value": "read" }
{ "type": "Dollar",     "value": 1 }      // $1
{ "type": "DollarZero"                  } // $0
```

### Sub-patch content

```json
{ "type": "inline",   ... canvas fields ... }   // resolved
{ "type": "unresolved" }                         // loader returned None
```
