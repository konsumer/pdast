# pdast

Convert PureData `.pd` patches to a JSON AST, and from there to other formats.

The project is a Rust workspace with four crates:

| Crate | What it does |
|---|---|
| `pdast` | Core library — parse `.pd` → AST, emit AST → `.pd`, JSON serialization |
| `pd2ast` | CLI — load a patch from disk (resolving abstractions) and print JSON |
| `ast2pd` | CLI — convert a JSON AST back to a `.pd` patch file |
| `pdast2faust` | CLI — read a JSON AST and generate Faust DSP code |

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

Create `<object-name>.dsp` in your lib directory. The file must define a `pdobj` identifier:

```faust
// fiddle~ — pitch tracker (custom implementation)
// Inlets: 1 (audio), Outlets: 1 (detected frequency)
import("stdfaust.lib");
pdobj = an.amp_follower(0.05) : *(440.0);  // stub — replace with real impl
```

The generator extracts the expression after `pdobj =` up to the first `;`. If the file contains `import("stdfaust.lib")`, the generator adds the import to the output automatically.

#### Built-in object coverage

| PD object | Faust equivalent |
|---|---|
| `osc~` | `os.osc` |
| `phasor~` | `os.phasor(1)` |
| `noise~` | `no.noise` |
| `*~` | `*` |
| `+~` | `+` |
| `-~` | `-` |
| `/~` | `/` |
| `lop~` | `fi.lowpass(1, freq)` |
| `hip~` | `fi.highpass(1, freq)` |
| `bp~` | `fi.resonbp(freq, Q, 1.0)` |
| `vcf~` | `fi.resonbp` split to 2 outlets |
| `delread~` / `vd~` | `de.delay` / `de.fdelay` |
| `line~` | `si.smooth` (approximation) |
| `sig~` | constant signal |
| `abs~` / `sqrt~` | `abs` / `sqrt` |
| `clip~` | `max(lo, min(hi, _))` |
| `dac~` / `adc~` | process outputs / inputs |
| `inlet~` / `outlet~` | sub-process I/O |

GUI objects (`hsl`, `vsl`, `nbx`, `tgl`, `bng`, `hradio`, `vradio`) are mapped to Faust UI primitives (`hslider`, `nentry`, `checkbox`, `button`).

Objects with no template emit a passthrough stub `_ // passthrough stub` and a warning.

### Limitations of Faust output

- Only audio-rate (tilde `~`) objects and GUI controls are translated. Pure control-rate logic (`metro`, `timer`, `route`, `select`, etc.) is not currently mapped.
- Complex feedback loops (`catch~`/`throw~`, `delwrite~`/`delread~` pairs) require manual review of the generated code.
- `tabread4~` and arrays generate placeholder stubs — you need to wire up the `rdtable` call manually.
- The generated `process` expression uses Faust's inline sequential/merge composition. For large patches with many fan-out connections the expression can be redundant (nodes may appear more than once). This is valid Faust but inefficient; refactoring to `letrec` or `with` blocks is a future improvement.

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
    "x": 0, "y": 50, "width": 450, "height": 300,
    "font_size": 12,
    "name": null,
    "open_on_load": false,
    "coords": null,
    "nodes": [
      {
        "id": 0,
        "x": 30, "y": 27,
        "kind": {
          "kind": "obj",
          "name": "osc~",
          "args": [{ "type": "Float", "value": 440.0 }]
        }
      }
    ],
    "connections": [
      { "src_node": 0, "src_outlet": 0, "dst_node": 1, "dst_inlet": 0 }
    ]
  }
}
```

### Node kind values

| `kind` field | Description |
|---|---|
| `"obj"` | Object box (vanilla or external) — has `name` and `args` |
| `"msg"` | Message box — has `messages` (array of arrays of tokens) |
| `"float_atom"` | Number box (`floatatom`) |
| `"symbol_atom"` | Symbol box (`symbolatom`) |
| `"text"` | Comment — has `content` |
| `"sub_patch"` | Inline sub-patch or resolved abstraction — has `name`, `args`, `content` |
| `"graph"` | Graph canvas (for arrays) — has `content` |
| `"gui"` | IEM GUI object — has `gui_kind`, `width`, `height`, `min`, `max`, etc. |
| `"array"` | Sample array — has `name`, `size`, `flags`, `data` |
| `"unknown"` | Unresolved external or broken box |

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
