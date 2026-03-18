# pdast — Development Notes

This document covers the internal design, data flow, known constraints, and guidance for extending the codebase. It is intended for contributors and for LLMs working on this project.

---

## Repository layout

```
pdast/                        ← Cargo workspace root
├── Cargo.toml                ← workspace manifest
├── Cargo.lock
├── PLAN.md                   ← original research + architecture plan
├── PROMPT.md                 ← original goal statement
├── README.md                 ← end-user documentation
├── DEVELOPMENT.md            ← this file
│
├── pdast/                    ← core library crate
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs            ← public API re-exports
│   │   ├── types.rs          ← all AST structs and enums
│   │   ├── error.rs          ← ParseError
│   │   ├── wasm.rs           ← WASM bindings (JSON ABI + wasm-bindgen JS API)
│   │   ├── parse/
│   │   │   ├── mod.rs        ← main parser (record splitter, canvas stack, element handlers)
│   │   │   ├── message.rs    ← atom tokenizer, escape handling
│   │   │   └── gui.rs        ← IEM GUI object parser and emitter
│   │   └── emit/
│   │       └── mod.rs        ← AST → .pd text emitter
│   └── tests/
│       └── integration.rs    ← integration tests against fixture files
│
├── pd2ast/                   ← CLI: .pd → JSON
│   └── src/main.rs
│
├── ast2pd/                   ← CLI: JSON AST → .pd
│   └── src/main.rs
│
├── pdast2faust/              ← CLI: JSON AST → Faust .dsp
│   ├── src/
│   │   ├── main.rs
│   │   └── faust_gen.rs      ← Faust code generator
│   └── faust-lib/            ← built-in Faust templates (one .dsp per PD object)
│       ├── osc~.dsp
│       ├── *~.dsp
│       └── ...
│
└── tests/
    └── fixtures/             ← shared .pd test patches
        ├── sine.pd
        ├── subpatch.pd
        ├── abstraction.pd
        ├── mygain.pd         ← abstraction used by abstraction.pd
        ├── gui.pd
        └── array.pd
```

---

## PureData file format summary

`.pd` files are plain text. Each record ends with `;\r\n` (CRLF; LF-only is tolerated). Three chunk types:

| Chunk | Meaning |
|---|---|
| `#N canvas x y w h [name open]` | Open a new canvas context. Root has 5 args (last = font size); sub-patch has 6 args (5th = name, 6th = open_on_load). |
| `#X <type> ...` | An element within the current canvas. |
| `#A <offset> <v1> <v2> ...` | Array sample data following a `#X array` record. |

### Object indexing

Every `#X` record that is **not** `connect` or `restore` is assigned an implicit sequential integer index (0-based) within its canvas. `#X connect` references these indices. This is critical: comments (`#X text`) ARE counted; `#X connect` and `#X restore` are NOT.

### Inline sub-patch lifecycle

```
#N canvas x y w h name open_on_load   ← parser pushes a new CanvasFrame onto the stack
#X obj ...                             ← nodes recorded in the new frame
#X connect ...
#X restore x y pd [name]              ← parser pops the frame, wraps it as SubPatch, pushes into parent
```

There is no separate `#X obj pd name` record. The `#X restore pd` record both terminates the sub-canvas AND places the node in the parent. The node's x/y come from `#X restore`, not `#N canvas`.

### Abstraction (external .pd file)

An abstraction is just `#X obj x y abstractionname arg1 arg2`. The `.pd` body is in a separate file. The parser detects this by calling the loader callback. If the loader returns content, it is recursively parsed and stored as `SubPatchContent::Inline`. If not, the node is stored as `NodeKind::Obj` (treated as a vanilla/external object).

### IEM GUI objects

All IEM GUI types (`bng`, `tgl`, `nbx`, `hsl`, `vsl`, `hradio`, `vradio`, `vu`, `cnv`) appear as `#X obj x y <name> <args...>` but have a fixed positional arg layout. The parser recognises them by name and routes to dedicated parsers in `parse/gui.rs`. Colors are stored as a single signed negative integer: `color = -(R*65536 + G*256 + B)`.

### Arrays

Arrays live inside a `graph` sub-canvas:

```
#N canvas ...                 ← opens graph context
#X array <name> <size> float <flags>  ← declares the array (node in graph canvas)
#A 0 v1 v2 ...                ← sample data for the most-recently declared array
#X coords ...                 ← coordinate system for the graph display
#X restore x y graph          ← closes graph, pushes Graph node into parent
```

The `flags` bitmask: bit 0 = save data in file, bits 1-2 = plot style, bit 3 = hide name.

---

## Parser internals (`pdast/src/parse/mod.rs`)

### Record splitter

`split_records()` is a character-by-character state machine that:

1. Watches for `#N`, `#X`, `#A` to start a record.
2. Passes `\\` escape sequences through verbatim (the atom parser handles them later).
3. Treats an unescaped `;` as the record terminator.
4. Collapses `\r\n` and `\n` inside a record body to a single space.

The result is a flat `Vec<Record>` with no nesting — nesting is reconstructed by the canvas stack.

### Canvas stack

`CanvasFrame` wraps a `Canvas` in progress, tracking:
- `next_id: u32` — the next object index to assign.
- `pending_array_id: Option<u32>` — the id of the most recent `#X array` node, so that subsequent `#A` records know where to write their data.

`parse_patch_dyn` (the internal entry point) iterates records and dispatches:

- `#N canvas` → push a new `CanvasFrame`.
- `#X restore pd` → pop the top frame, wrap its canvas in `NodeKind::SubPatch`, push into parent frame. **Does not consume an object index.**
- `#X restore graph` → same but wraps in `NodeKind::Graph`.
- `#X connect` → append to parent frame's `connections`. **Does not consume an object index.**
- All other `#X` types → call the relevant handler, which calls `frame.push_node(x, y, kind)`. This increments `next_id` and appends to `frame.canvas.nodes`.
- `#A` → look up `frame.pending_array_id`, find the node, append data.

### Recursive abstraction loading and the monomorphization problem

The public `parse_patch<F>()` is generic over `F: Fn(&str) -> Option<String>`. The internal `parse_patch_dyn()` takes `&dyn Fn(...)` to avoid infinite monomorphization depth when calling recursively for abstractions. The public function immediately calls the private one: `parse_patch_dyn(content, &loader)`.

### Object dispatch in `handle_obj`

1. Check `try_parse_gui(name, &raw_args)` → if matched, emit `NodeKind::Gui`.
2. Call `loader(name)` → if `Some(content)`, recursively parse and emit `NodeKind::SubPatch { content: Inline(...) }`.
3. Otherwise emit `NodeKind::Obj { name, args }`. This covers both vanilla PD objects and unresolved externals; the distinction is left to downstream consumers.

### Atom tokenizer (`parse/message.rs`)

`parse_atom(s)` converts a single whitespace-split token string into a `Token`:

- Starts with `$0` → `Token::DollarZero`
- Starts with `$N` (N parseable as u32) → `Token::Dollar(N)`
- Parseable as f64 → `Token::Float(f)`
- Otherwise → `Token::Symbol(unescape_symbol(s))`

Escape handling in `unescape_symbol`: `\;` → `;`, `\,` → `,`, `\\` → `\`. These escapes are passed through verbatim by the record splitter and resolved here.

`emit_token` is the inverse: floats are emitted without trailing `.0` when whole (using `as i64` formatting), symbols have `;`, `,`, `\`, and spaces re-escaped.

---

## AST types (`pdast/src/types.rs`)

### Serde tag strategy

`NodeKind` uses `#[serde(tag = "kind", rename_all = "snake_case")]` — an internally-tagged enum. This means the discriminant is inlined as a `"kind"` field in the JSON object.

**Collision avoidance:** `GuiObject` has a `kind: GuiKind` field. Because `NodeKind::Gui(GuiObject)` is internally tagged, both `NodeKind` (`"kind": "gui"`) and `GuiObject.kind` would serialize to the same `"kind"` key, causing a duplicate field. This is resolved by:

- `GuiObject.kind` is renamed in serde: `#[serde(rename = "gui_kind")]`
- `GuiExtra` uses `#[serde(tag = "extra_kind")]` (not `"kind"`) for the same reason.

### SubPatchContent

```rust
pub enum SubPatchContent {
    Inline(Box<Canvas>),   // type: "inline"
    Unresolved,            // type: "unresolved"
}
```

Uses `#[serde(tag = "type", rename_all = "snake_case")]`. The `Inline` variant wraps `Box<Canvas>` to avoid infinite type recursion (Canvas contains Nodes which can contain SubPatchContent which contains Canvas).

### Color

PD stores colors as `-(R*65536 + G*256 + B)`. `Color::from_pd_int` and `Color::to_pd_int` handle encoding and decoding. Both positive and negative inputs are accepted in `from_pd_int` to handle PD ≥0.52's format.

---

## Emitter (`pdast/src/emit/mod.rs`)

`emit_patch` walks the AST recursively. Key points:

- For `SubPatch { content: Inline(inner) }`: the inner canvas is emitted first (its `#N canvas` record opens it), then `#X restore x y pd name` closes it in the parent. This is the correct PD file structure.
- For `SubPatch { content: Unresolved }`: emitted as a plain `#X obj` since the body is unavailable.
- For `Graph { content }`: same recursive pattern with `#X restore x y graph`.
- Object indices in `#X connect` records are determined by the `node.id` fields, which were assigned during parsing and preserved in the AST. No re-counting is needed.
- `#X coords` is emitted before `#X connect` records for a given canvas, which matches PD's own ordering.
- Array data is emitted in chunks of 1000 values to match PD's convention.
- CRLF line endings (`\r\n`) are used throughout, matching the PD spec.

### Roundtrip fidelity

`parse(emit(parse(input)))` produces the same AST as `parse(input)`. The emitted text may differ from the original in whitespace, float formatting, and field ordering, but the AST is identical. Tests in `emit/mod.rs` and `tests/integration.rs` verify this.

---

## pd2ast CLI (`pd2ast/src/main.rs`)

Straightforward: reads the root patch, builds a search-path list (patch dir + `-p` args), constructs a loader closure that walks the list and reads files, calls `parse_patch`, serializes to JSON.

The `loading: HashSet<String>` variable was intended for cycle detection but is not effective inside a `Fn` closure (can't mutate captured `&mut`). Cycle detection is left as a future improvement; practical PD patches do not have self-referencing abstractions.

---

## ast2pd CLI (`ast2pd/src/main.rs`)

A thin wrapper around `pdast::from_json` + `pdast::emit_patch`. It reads a JSON AST from a file or stdin (`-`), deserialises it into a `Patch`, calls `emit_patch`, and writes the resulting `.pd` text to a file or stdout.

The tool is intentionally minimal — all the work is done by the library. Its primary purpose is to close the `pd2ast | ast2pd` roundtrip and to allow any JSON-aware tool (jq, JavaScript, Python, etc.) to sit in the middle and transform the AST before converting back to a patch.

### Roundtrip guarantee

`parse(emit(parse(input)))` produces the same `Patch` value as `parse(input)`. This is verified by four integration tests in `pdast/tests/integration.rs` under the `test_json_pd_roundtrip_*` family. The tests follow the exact same path as the CLI pipeline:

```
.pd source
  → parse_patch_no_loader  (parse)
  → to_json                (pd2ast step)
  → from_json              (ast2pd step)
  → emit_patch             (ast2pd step)
  → parse_patch_no_loader  (re-parse to compare)
  → assert_eq!(r1.patch, r3.patch)
```

### What is not preserved verbatim

The emitted text differs cosmetically from the original source (see README for the full list), but the AST parsed from it is identical. PureData itself is tolerant of all these differences.

---

## pdast2faust — Faust generator (`pdast2faust/src/faust_gen.rs`)

### Template system

Each PD object type has a `.dsp` template file. The convention:

```faust
// comment / metadata
import("stdfaust.lib");   // optional
pdobj[(params)] = <faust-expression>;
```

`parse_template` extracts everything after `pdobj` (including the optional parameter list) up to the first `;` at depth 0. This preserves parameter declarations like `(ms) = ba.pulse(...)` intact.

The helper is emitted as:

```faust
pd_metro(ms) = ba.pulse(max(1, int(ba.ms2samp(ms))));
```

At call sites, PD creation arguments are appended as partial application: `pd_metro(500)`.

`TemplateResolver` caches resolved templates. User dirs are searched before the built-in lib. The built-in lib covers ~60 objects and is embedded at compile time via `include_str!` macros in `builtin_lib()`.

Object names are sanitized to valid Faust identifiers with `sanitize_name`. Operators use explicit mappings: `*` → `mul`, `/` → `div`, `>` → `gt`, etc. Tilde suffix: `~` → `s` (so `osc~` → `pd_oscs`).

### Unified graph model

The generator processes **all nodes** — both audio-rate and control-rate — in a single unified graph. There is no pre-filtering by `is_signal`. Every PD node (objects, GUI, message boxes, atoms) that can contribute a value participates.

`topo_sort` implements Kahn's BFS algorithm over all active node ids. Cycle nodes (feedback loops — e.g. `float` fed by `+`) are appended after the BFS result; they produce Faust code with implicit one-sample feedback delay which is correct for audio.

### Named `with { }` bindings

Each node in topological order gets a named Faust binding `n<id>`:

```faust
process = n9
with {
  n0 = pd_metro(500);
  n1 = n2 : pd_float(0);
  n2 = n1 : pd_p(1);
  ...
};
```

This means fan-out connections (one outlet → multiple inlets) do not duplicate computation — every node is computed exactly once. The `with { }` pattern is idiomatic Faust for this purpose.

### Multi-outlet nodes

For nodes that produce more than one output (e.g. `moses` → 2 outlets, `notein` → 3), the generator selects the correct outlet using:

```faust
(src_expr <: si.bus(N)) : ba.selector(outlet_idx, N)
```

### send / receive bus resolution

`collect_bus_map` scans the canvas for all `[send X]`, `[receive X]`, and `[value X]` nodes and groups them by name. For each pair:

- The **send** node is emitted as `_` (passthrough) — its binding holds the value.
- The **receive** node's RHS is set directly to the send node's binding name.
- A send node paired with a receive is excluded from the sink list (it is an internal bus, not an output).
- An unpaired receive emits `nentry("name", ...)` — a UI control with the same name.

Cross-canvas send/receive (different canvases or abstractions) is not yet supported.

### Node RHS construction (`node_rhs`)

The RHS for each binding is built by `node_rhs`:

- `NodeKind::Gui` → `gui_to_faust` → Faust UI primitive
- `NodeKind::Msg` → first numeric atom as a constant
- `NodeKind::FloatAtom` / `SymbolAtom` → `nentry`
- `NodeKind::SubPatch::Inline` → the inner `process = ...` expression is extracted and inlined
- `NodeKind::Obj` → `apply_fn` → `pd_<name>(creation_args)` wired with incoming connections

Incoming connections are sorted by inlet index. For a single input: `src : fn`. For multiple: `(src1, src2) : fn`.

### GUI → Faust UI mapping

| PD GUI | Faust UI |
|---|---|
| `hsl`, `vsl` | `hslider("label", default, min, max, 0.001)` |
| `tgl` | `checkbox("label")` |
| `nbx` | `nentry("label", default, min, max, 0.001)` |
| `bng` | `button("label")` |
| `hradio`, `vradio` | `nentry("label", default, min, max, 1)` |
| `vu`, `cnv` | comment-only (decorative) |

---

## Known issues and future work

### Parser

- **Cycle detection in abstraction loading**: the loader `Fn` can't mutate a `HashSet` to track in-progress loads. If a patch `A.pd` uses abstraction `B.pd` which uses `A.pd`, the parser will infinite-recurse. Fix: use a `RefCell<HashSet>` or convert the loader signature to pass a context parameter.
- **`#X declare`**: PD's `[declare -path ...]` and `[declare -lib ...]` are not parsed or acted on. They appear as `NodeKind::Obj { name: "declare", ... }`. The pd2ast CLI's `-p` flags are the intended substitute.
- **`$0`-expanded names**: symbol atoms containing `$0-myname` are stored literally with `Token::DollarZero`; expansion is not performed at parse time.

### Emitter

- **Float formatting**: very large or very small floats may not round-trip identically (e.g. `1e+037` vs `10000000000000000000000000000000000000`). PD is generally tolerant of this.
- **GUI color format**: PD ≥0.52 uses a different color format. The emitter always outputs the legacy negative-integer format, which older and newer PD both accept.

### Faust generator

- **`delwrite~`/`delread~` pairing**: these are treated as independent nodes. Pair them manually by sharing a `de.delay` instance in a custom template.
- **`tabread4~` / arrays**: the generator emits a passthrough stub. A full implementation would collect `Array` nodes from the AST, emit `rdtable` declarations, and wire `tabread4~` to them.
- **Sub-patch creation argument substitution**: `$1`, `$2` inside an inlined abstraction are not substituted with the creation arguments supplied at the call site. The `args` field on `NodeKind::SubPatch` holds the values but the substitution step is not yet implemented.
- **Cross-canvas send/receive**: `[send X]` in one canvas and `[receive X]` in another (or in a sub-patch) are not wired together. Each canvas is processed independently.
- **`expr` / `expr~`**: PD's C-style expression language is not parsed. These nodes emit a passthrough stub and a warning.

### WASM (`pdast/src/wasm.rs`)

WASM support is implemented in two layers.

#### Layer 1 — JSON string API (always compiled for `wasm32`)

The functions `wasm_parse_to_json`, `wasm_emit_to_pd`, and `wasm_patch_to_pd` operate entirely on `&str`/`String` and require no JS-specific types. They are compiled whenever `target_arch = "wasm32"` (both `wasm32-unknown-unknown` and `wasm32-wasip1`).

Abstractions cannot be supplied via a callback (WASM has no general function-pointer mechanism across the host boundary), so they are passed as a JSON object string `{"name": "content", ...}`. The helper `make_loader_map` parses this into a `HashMap` which is then used as the loader closure.

Alongside these high-level functions, three `extern "C"` ABI functions (`wasm_parse_to_json_abi`, `wasm_emit_to_pd_abi`, `wasm_patch_to_pd_abi`) are exported with `#[unsafe(no_mangle)]`. They follow a ptr+len convention: inputs are passed as `(*const u8, u32)` pairs; the output is a freshly allocated buffer encoded as `(ptr as i64) << 32 | len as i64`. The host must free the result buffer by calling the also-exported `wasm_alloc` / `wasm_dealloc` pair.

#### Layer 2 — JS-host API (`feature = "wasm-js"`)

When compiled with `--features wasm-js`, the `js` submodule activates. It uses `wasm-bindgen`, `serde-wasm-bindgen`, and `js-sys`. Four `#[wasm_bindgen]` functions are exported:

| JS name | Rust function | Notes |
|---|---|---|
| `parse` | `js_parse` | Returns JS object; optional `Function` loader |
| `parseToJson` | `js_parse_to_json` | Returns JSON string; optional `Function` loader |
| `emitPatch` | `js_emit` | Accepts JS object (Patch or ParseResult) |
| `emitPatchFromJson` | `js_emit_from_json` | Accepts JSON string |

The loader is a `js_sys::Function` called as `loader(name)` returning a string or null/undefined.

`serde-wasm-bindgen::to_value` / `from_value` handle the Rust↔JS object conversion. The `js_emit` function tries to deserialise the JS value as a bare `Patch` first, then falls back to `ParseResult` (unwrapping `.patch`), so both shapes work.

#### Feature flags and targets summary

| Command | Target | Feature | Output |
|---|---|---|---|
| `wasm-pack build pdast --features wasm-js` | `wasm32-unknown-unknown` | `wasm-js` | `pdast/pkg/` npm package |
| `cargo build -p pdast --target wasm32-unknown-unknown --features wasm-js` | same | `wasm-js` | raw `.wasm` + bindgen glue |
| `cargo build -p pdast --target wasm32-wasip1` | WASI | none | WASI `.wasm`, C ABI exports only |

#### `pkg/` directory

`wasm-pack build` produces `pdast/pkg/` containing:
- `pdast.js` — ESM entry point
- `pdast_bg.wasm` — the compiled module
- `pdast_bg.js` — low-level JS glue generated by `wasm-bindgen`
- `pdast.d.ts` — TypeScript declarations
- `package.json` — npm package metadata

This directory is listed in `.gitignore`; it should be (re)built before publishing to npm. The `pkg/` output is an ESM module consumable directly in modern browsers, Node.js (v18+), Deno, and Bun.

#### Edition 2024 notes

Edition 2024 changed two things that affect the WASM code:
- `#[no_mangle]` must be written `#[unsafe(no_mangle)]` — done.
- `unsafe fn` bodies require an explicit `unsafe {}` block for unsafe operations — done in `wasm_dealloc` and the ABI helpers.

---

## Adding a new code generator

To add a new output target (e.g. Teensy Audio Library, C++, RNBO):

1. Create a new binary crate: `cargo new myformat-target`
2. Add it to the workspace `Cargo.toml` under `members`.
3. Depend on `pdast`: `pdast = { path = "../pdast" }`.
4. Read the JSON AST from stdin or a file using `pdast::from_json`.
5. Walk `patch.root.nodes` and `patch.root.connections`.
6. For objects with sub-patches (`NodeKind::SubPatch { content: SubPatchContent::Inline(canvas), .. }`), recurse into the inner canvas.
7. Use `topo_sort` logic (copy from `pdast2faust/src/faust_gen.rs` or factor it into `pdast` lib) to get evaluation order.
8. Emit target-specific code.

The Teensy Audio Library generator (planned) would map:

| PD | Teensy class |
|---|---|
| `osc~` (sine) | `AudioSynthWaveformSine` |
| `noise~` | `AudioSynthNoisePink` |
| `*~` (multiply) | `AudioEffectMultiply` |
| `+~` (fan-in) | `AudioMixer4` |
| `lop~` / `hip~` | `AudioFilterStateVariable` |
| `dac~` | `AudioOutputI2S` |
| `adc~` | `AudioInputI2S` |

The generator would emit:
1. Object declarations: `AudioSynthWaveformSine sine1;`
2. `AudioConnection` wiring: `AudioConnection c0(sine1, 0, mixer1, 0);`
3. A `setup()` function configuring initial parameters.
4. `AudioMemory(N)` with an estimated buffer count.

---

## Running tests

```sh
cargo test --workspace              # all crates
cargo test -p pdast                 # library unit + integration tests only
cargo test -p pdast -- --nocapture  # with stdout
```

Test fixtures live in `tests/fixtures/`. The integration tests use `env!("CARGO_MANIFEST_DIR")` to locate them regardless of working directory.

### Test inventory

| Test group | Count | What it covers |
|---|---|---|
| `parse::message::tests` | 5 | Atom tokenizer: floats, dollars, symbols, escapes |
| `parse::tests` | 8 | Parser: all node types, indexing, connections, sub-patches, loader, arrays |
| `emit::tests` | 3 | Emitter roundtrips: simple, msg box, sub-patch |
| `integration` — parsing | 5 | Fixture files: sine, sub-patch, abstraction loader, GUI objects, arrays |
| `integration` — JSON roundtrip | 2 | `parse → to_json → from_json` identity |
| `integration` — PD roundtrip | 3 | `parse → emit_patch → re-parse` identity |
| `integration` — JSON+PD roundtrip | 4 | `parse → JSON → from_json → emit → re-parse` (full `pd2ast \| ast2pd` path) |
| Doc-test | 1 | `lib.rs` code example |
| **Total** | **31** | |

---

## Code style notes

- No `unwrap()` in library code paths that handle user input; prefer `?` or emit a `Warning` and continue.
- Warnings are non-fatal and accumulated into `Vec<Warning>` — never panic on malformed input.
- All public types derive `Debug`, `Clone`, `PartialEq`, `Serialize`, `Deserialize`.
- Internal parser helpers (`split_first_word`, `parse_xy`, `get_arg`, `opt_str_pd`) are private to `parse/mod.rs`.
- The emitter and parser are independent modules; neither imports the other.
- Faust template files in `faust-lib/` are embedded at compile time with `include_str!` — no runtime file I/O is required for the built-in library.
