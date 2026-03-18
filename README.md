# pdast

Convert PureData `.pd` patches to a JSON AST, and from there to other formats.

The project is a Rust workspace with four crates:

| Crate         | What it does                                                           |
| ------------- | ---------------------------------------------------------------------- |
| `pdast`       | Core library — parse `.pd` → AST, emit AST → `.pd`, JSON serialization |
| `pd2ast`      | CLI — load a patch from disk (resolving abstractions) and print JSON   |
| `ast2pd`      | CLI — convert a JSON AST back to a `.pd` patch file                    |
| `pdast2faust` | CLI — read a JSON AST and generate Faust DSP code                      |

## Installation

```sh
cargo install --path pd2ast
cargo install --path ast2pd
cargo install --path pdast2faust
```

Or build everything without installing:

```sh
cargo build --release
# binaries at target/release/pd2ast, target/release/ast2pd, target/release/pdast2faust
```

## pd2ast

Convert a `.pd` file to a JSON AST.

```
pd2ast [OPTIONS] <PATCH.pd>

Options:
  -p, --path <DIR>      Extra search path for abstractions (repeatable)
  -o, --output <FILE>   Write JSON to FILE instead of stdout
  -q, --quiet           Suppress warnings
      --compact         Minified JSON output
      --include-warnings  Include the warnings array in the JSON output
```

### Basic use

```sh
pd2ast my-patch.pd
pd2ast my-patch.pd > my-patch.json
pd2ast my-patch.pd -o my-patch.json
```

### Abstractions

pd2ast resolves abstractions (external `.pd` files referenced by name) the same way PureData does: it searches the patch's own directory first, then any extra `-p` paths.

```sh
pd2ast my-patch.pd -p ~/pd-externals -p ~/pd-abstractions
```

If an abstraction cannot be found, the object is stored as `unknown` in the AST and a warning is printed to stderr. Use `--quiet` to suppress warnings.

### Compact output

```sh
pd2ast my-patch.pd --compact
```

### Including warnings in the output

```sh
pd2ast my-patch.pd --include-warnings
```

### Pipeline use

```sh
pd2ast my-patch.pd | jq '.root.nodes[] | select(.kind.kind == "obj")'
```

## ast2pd

Convert a JSON AST (from `pd2ast`) back to a PureData `.pd` patch file.

```
ast2pd [OPTIONS] <AST.json | ->

Options:
  -o, --output <FILE>   Write .pd output to FILE instead of stdout
```

### Basic use

```sh
ast2pd my-patch.json
ast2pd my-patch.json -o out.pd
```

### Reading from stdin

Use `-` as the input path to read from stdin, enabling pipeline use:

```sh
ast2pd - < my-patch.json
pd2ast my-patch.pd | ast2pd -
```

### Full roundtrip

Convert a patch to JSON, manipulate it (with `jq` or any other tool), then convert back:

```sh
# Simple roundtrip — output should be semantically identical to input
pd2ast my-patch.pd | ast2pd - -o roundtripped.pd

# Manipulate in the middle — e.g. remove all comment nodes
pd2ast my-patch.pd \
  | jq 'del(.root.nodes[] | select(.kind.kind == "text"))' \
  | ast2pd - -o no-comments.pd
```

The roundtrip preserves the full patch structure: all nodes, connections, sub-patches, GUI objects, arrays, and abstraction bodies (when resolved by `pd2ast`). Position and size information is preserved exactly.

### What changes in the roundtrip

The emitted `.pd` text may differ from the original source in these cosmetic ways — none affect how PureData loads the file:

- Line endings are always CRLF (`\r\n`), regardless of the input.
- Floating-point numbers are re-formatted (e.g. `1e+037` may become the full decimal integer).
- Whitespace within records is normalised to single spaces.
- The order of `#X coords` records relative to `#X connect` records within a canvas is fixed (coords always precede connections in emitted output).

## pdast2faust

Convert a JSON AST (from `pd2ast`) to a [Faust](https://faust.grame.fr/) `.dsp` file.

```
pdast2faust [OPTIONS] <AST.json | ->

Options:
  -L, --lib <DIR>       Extra library directory for object templates (repeatable)
  -o, --output <FILE>   Write Faust code to FILE instead of stdout
  -q, --quiet           Suppress warnings
```

### Basic use

```sh
pd2ast my-patch.pd | pdast2faust -
pdast2faust my-patch.json
pdast2faust my-patch.json -o my-patch.dsp
```

### Full pipeline: patch → JSON → Faust

```sh
pd2ast my-patch.pd | pdast2faust - -o my-patch.dsp
faust -o my-patch.cpp my-patch.dsp          # compile to C++
faust2jaqt my-patch.dsp                     # build a JACK standalone app
faust2lv2 my-patch.dsp                      # build an LV2 plugin
```

### Object templates (lib dirs)

Each supported PD object has a `.dsp` template file named `<object-name>.dsp`. A built-in library covers common vanilla PD DSP objects. You can add your own for externals or custom abstractions.

```sh
# Use a custom lib dir to support fiddle~ (or any other object)
pdast2faust my-patch.json -L ~/my-faust-lib

# Multiple lib dirs, searched in order before the built-in library
pdast2faust my-patch.json -L ./project-lib -L ~/global-lib
```

#### Writing a template file

Create `<object-name>.dsp` in your lib directory. The file must define a `pdobj` identifier, optionally with parameters matching PD's creation arguments:

```faust
// fiddle~ — pitch tracker (custom implementation)
// Inlets: 1 (audio), Outlets: 1 (detected frequency)
import("stdfaust.lib");
pdobj = an.amp_follower(0.05) : *(440.0);  // stub — replace with real impl

// lop~ <freq>  — one-pole lowpass with creation-arg frequency
pdobj(freq) = fi.lowpass(1, max(1.0, freq));
```

The generator emits `pd_name(params) = <expr>;` preserving the parameter list exactly. When the PD patch contains creation arguments (e.g. `[lop~ 500]`), they are passed as `pd_lops(500)` at the call site.

#### Built-in object coverage

**Audio-rate (tilde) objects**

| PD object                      | Faust equivalent              | Notes                |
| ------------------------------ | ----------------------------- | -------------------- |
| `osc~`                         | `os.osc`                      | Sine oscillator      |
| `phasor~`                      | `os.phasor(1)`                | 0–1 sawtooth         |
| `noise~`                       | `no.noise`                    | White noise          |
| `*~` `+~` `-~` `/~`            | `*` `+` `-` `/`               | Arithmetic           |
| `lop~`                         | `fi.lowpass(1, freq)`         | One-pole LP          |
| `hip~`                         | `fi.highpass(1, freq)`        | One-pole HP          |
| `bp~`                          | `fi.resonbp(freq, Q, 1)`      | Bandpass             |
| `vcf~`                         | `fi.resonbp` → 2 outlets      |                      |
| `biquad~`                      | `fi.tf2(b0,b1,b2,a1,a2)`      | Direct-form II       |
| `rzero~` / `rpole~`            | FIR/IIR one-pole              |                      |
| `delread~` / `vd~`             | `de.delay` / `de.fdelay`      | Fixed / interpolated |
| `line~`                        | `si.smooth`                   | Exponential approx.  |
| `sig~`                         | constant signal               |                      |
| `abs~` `sqrt~` `wrap~` `clip~` | `abs` `sqrt` `ma.frac` `clip` |                      |
| `dac~` / `adc~`                | process outputs / inputs      |                      |
| `inlet~` / `outlet~`           | sub-process I/O               |                      |
| `snapshot~`                    | `ba.sAndH` on rising edge     |                      |
| `samphold~`                    | `ba.sAndH`                    |                      |
| `env~`                         | `an.amp_follower_ud`          | RMS follower         |
| `threshold~`                   | `ef.gate_mono`                | Schmitt trigger      |
| `expr~`                        | passthrough stub + warning    | Needs manual edit    |
| `tabread4~` / `tabosc4~`       | passthrough / `os.osc` stub   | Needs `rdtable`      |

**Control-rate objects**

| PD object                               | Faust equivalent                  | Notes                     |
| --------------------------------------- | --------------------------------- | ------------------------- |
| `+` `-` `*` `/`                         | Inline math                       | Always-running            |
| `mod` `pow` `max` `min`                 | `fmod` `pow` `max` `min`          |                           |
| `abs` `sqrt` `log` `exp`                | Built-ins                         |                           |
| `sin` `cos` `atan` `atan2`              | Built-ins                         |                           |
| `wrap` `clip` `int`                     | `ma.frac` `clip` `int`            |                           |
| `>` `<` `>=` `<=` `==` `!=`             | Comparison operators → float      |                           |
| `&&` `\|\|` `!`                         | `&` `\|` `==(0)`                  |                           |
| `change`                                | `x != x'`                         | Compare to prev sample    |
| `moses`                                 | `x*(x<N), x*(x>=N)`               | Two outputs               |
| `sel` / `select`                        | `==(target)` boolean mask         |                           |
| `metro`                                 | `ba.pulse(ba.ms2samp(N))`         | Block-aligned approx.     |
| `line`                                  | `si.smooth`                       | Exponential approx.       |
| `delay` / `pipe`                        | `de.delay` on trigger             |                           |
| `timer`                                 | Sample counter                    | Approx.                   |
| `bang`                                  | `button` + rising edge            |                           |
| `float` / `int`                         | `ba.sAndH`                        | Sample-and-hold approx.   |
| `send` / `receive`                      | Shared binding                    | Direct wire within canvas |
| `value`                                 | `nentry` or shared binding        |                           |
| `mtof` / `ftom`                         | `ba.midikey2hz` / `ba.hz2midikey` |                           |
| `dbtorms` `rmstodb` `dbtopow` `powtodb` | Math expressions                  |                           |
| `notein` `ctlin` `bendin`               | Faust MIDI UI metadata            |                           |
| `pack` / `unpack`                       | Parallel signals                  | Numeric only              |
| `trigger` / `t`                         | Simultaneous outputs              | Ordering lost — see below |
| `expr`                                  | passthrough stub + warning        | Needs manual edit         |

**GUI objects** (`hsl`, `vsl`, `nbx`, `tgl`, `bng`, `hradio`, `vradio`) map to Faust UI primitives (`hslider`, `nentry`, `checkbox`, `button`).

Objects with no template emit `_` (passthrough) with a warning.

### Code generation model

The generator produces a Faust `with { }` block where each PD node becomes a named binding (`n0`, `n1`, …). This means:

- Fan-out connections (one outlet → many inlets) are handled without duplicating computation.
- Control-rate and audio-rate nodes are mixed freely in the same graph.
- Every node is computed on every sample (always-on), which is the correct Faust model.

### Semantic caveats

| PD concept                      | Faust approximation                 | What's lost                                               |
| ------------------------------- | ----------------------------------- | --------------------------------------------------------- |
| `metro` wall-clock timing       | Block-aligned `ba.pulse`            | Slight drift; a 1ms metro fires every block, not every ms |
| `float` / `int` storage         | `ba.sAndH` always-on                | Bang → output becomes always-outputting                   |
| `trigger` / `t` ordering        | Simultaneous outputs                | Right-to-left outlet firing order is not preserved        |
| `send` / `receive`              | Direct binding wire (within canvas) | Cross-patch buses not supported                           |
| `route` (by type/symbol)        | Not supported                       | Symbol routing has no Faust equivalent                    |
| `pack` / `unpack` (mixed types) | Numeric fields only                 | Symbol fields dropped                                     |
| `expr` / `expr~`                | Passthrough stub                    | PD's C-style expression language needs manual conversion  |

### Limitations of Faust output

- `tabread4~` and arrays generate placeholder stubs — wire up `rdtable` manually for wavetable playback.
- Feedback loops through control objects (`float` driving itself via `+`) produce a one-sample Faust feedback delay (`~`), which is correct for audio but may produce subtle ordering differences for control logic.
- `delwrite~`/`delread~` pairs are treated as independent nodes. Pair them manually by sharing a `de.delay` instance.
- Sub-patches are inlined as expressions, but creation-argument substitution (`$1`, `$2` → values) is not yet performed.

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

## Running tests

```sh
cargo test --workspace
```

The test suite covers: parsing all node types, object ID assignment, connections, inline sub-patches, abstraction loading, GUI objects, arrays, PD roundtrip (parse → emit → re-parse), and JSON roundtrip.
