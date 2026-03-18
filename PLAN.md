# pdast — PureData AST Library: Research & Implementation Plan

## Overview

`pdast` is a Rust library that parses PureData `.pd` patch files into a JSON-serializable AST, and can serialize the AST back to `.pd`. The library targets:

- Native Rust (library crate)
- WebAssembly (both JS hosts via `wasm-bindgen` and non-JS WASM hosts)
- Downstream code generators (examples: PD roundtrip, Faust, Teensy Audio Library C++)

---

## 1. PureData File Format Summary

### 1.1 File Structure

`.pd` files are plain-text, record-based. Each record ends with `;\r\n` (CRLF). Three chunk types exist:

| Chunk | Role                                               |
| ----- | -------------------------------------------------- |
| `#N`  | New canvas declaration (root or sub-patch)         |
| `#X`  | Object, message, connection, restore, coords, etc. |
| `#A`  | Array data                                         |

### 1.2 Root Canvas

Every `.pd` file starts with exactly one root canvas:

```
#N canvas <x> <y> <width> <height> <font_size>;
```

### 1.3 Sub-patch Canvas

Opened inline:

```
#N canvas <x> <y> <width> <height> <name> <open_on_load>;
```

Closed and placed in parent:

```
#X restore <x> <y> pd [name];
```

`#X restore` is NOT counted in the object index.

### 1.4 Object Numbering

Every `#X` element that is NOT `connect` or `restore` receives an implicit sequential integer index (0-based), in file order. `#X connect` uses these indices to wire objects together. `#X text` (comments) ARE counted.

### 1.5 Element Types

| Element      | Syntax                                                                                                  |
| ------------ | ------------------------------------------------------------------------------------------------------- |
| `obj`        | `#X obj <x> <y> <name> [args...];`                                                                      |
| `msg`        | `#X msg <x> <y> [tokens...];`                                                                           |
| `floatatom`  | `#X floatatom <x> <y> <width> <min> <max> <label_pos> <label> <receive> <send>;`                        |
| `symbolatom` | Same as floatatom                                                                                       |
| `text`       | `#X text <x> <y> <comment...>;`                                                                         |
| `connect`    | `#X connect <src_idx> <src_outlet> <dst_idx> <dst_inlet>;`                                              |
| `restore`    | `#X restore <x> <y> <type> [name];`                                                                     |
| `coords`     | `#X coords <x_from> <y_top> <x_to> <y_bottom> <width_px> <height_px> <gop_flag> [x_margin] [y_margin];` |
| `#A`         | `#A <start_index> <val1> <val2> ...;`                                                                   |

### 1.6 IEM GUI Objects

GUI objects (bng, tgl, nbx, hsl, vsl, hradio, vradio, vu, cnv) are `#X obj` records with a recognized name and many positional parameters including colors (negative-integer encoded RGB), send/receive symbol names, and labels. They ARE counted in the object index.

### 1.7 Arrays

Arrays live inside a graph sub-canvas:

```
#N canvas 0 0 450 300 graph1 0;
#X array <name> <size> float <flags>;
#A 0 <val1> <val2> ...;
#X coords ...;
#X restore <x> <y> graph;
```

Array flags (bitmask): bit0=save data, bits1-2=plot style, bit3=hide name.

### 1.8 Abstractions vs. Inline Sub-patches

- **Inline sub-patch**: The full patch content is embedded in the file between `#N canvas ... name` and `#X restore`.
- **Abstraction**: `#X obj x y my-abstraction arg1 arg2;` — the body is in a separate `my-abstraction.pd` file. Only the name and creation args are stored.

The parser cannot distinguish an abstraction from a vanilla/external object unless it resolves the name (either to a known built-in, or via the user-supplied loader callback).

### 1.9 Escaping Rules

In message content: `,` separates list items; `;` terminates. Literals: `\,` and `\;`. Backslash: `\\`. Dollar args: `$1`, `$2`, ..., `$0` (instance ID).

### 1.10 Coordinate System

Screen coordinates: origin top-left, X right, Y down, all pixels. The `coords` element defines a data coordinate system (separate from pixel coords) for graphs.

### 1.11 Colors

Stored as a signed negative integer: `color = -(R*65536 + G*256 + B)`. Decode: `R = ((-c) >> 16) & 0xFF`, etc. Common: `-262144` = black, `-1` = white. Newer PD (≥0.52) may use plain positive decimal RGB — both must be handled.

### 1.12 Externals

Externals are compiled shared libraries. In the `.pd` file they are syntactically identical to vanilla objects. They are unknown at parse time unless resolved via the loader callback or an explicit catalog.

---

## 2. Design Decisions

### 2.1 Abstraction/Sub-patch Loading

Use a **callback closure** passed to the parse function:

```rust
pub fn parse_patch<F>(content: &str, loader: F) -> Result<Patch, ParseError>
where
    F: Fn(&str) -> Option<String>
```

When the parser encounters an object name that could be an abstraction, it calls `loader(name)`. If `loader` returns `Some(content)`, the abstraction is recursively parsed and inlined into the AST. If `None`, the object is marked `NodeKind::Unknown`.

This works without a filesystem, and works identically in native and WASM contexts.

### 2.2 Unknown Objects

Unknown objects (unresolved abstractions, externals, broken boxes) are stored as `NodeKind::Unknown { name: Option<String>, args: Vec<Token> }`. Connections to/from them are preserved. Downstream tools decide how to handle them.

### 2.3 Validation

No structural validation during parsing. Warnings (wrong number of args, dangling connections, etc.) are collected in a `Vec<Warning>` and returned alongside the AST. The parse result type is:

```rust
pub struct ParseResult {
    pub patch: Patch,
    pub warnings: Vec<Warning>,
}
```

### 2.4 Serialization

Use `serde` + `serde_json` for JSON. All AST types derive `Serialize` + `Deserialize`. This enables:

- `serde_json::to_string(&ast)` → JSON string
- `serde_json::from_str(json)` → AST
- Other serde-compatible formats (MessagePack, CBOR) via feature flags later if desired

### 2.5 WASM Strategy

The library is compiled to WASM with `wasm-bindgen`. For JS hosts, `serde-wasm-bindgen` returns JS objects directly. For non-JS WASM hosts (WASI, component model, custom runtimes), JSON string interchange is available via a `parse_to_json(input: &str) -> String` function that requires no `wasm-bindgen`. Both interfaces are exposed.

---

## 3. AST Design

### 3.1 Token Type

PD message atoms are typed at parse time where unambiguous:

```rust
pub enum Token {
    Float(f64),
    Symbol(String),    // also covers escaped \; \, etc.
    Dollar(u32),       // $1, $2, ... positional
    DollarZero,        // $0 — instance ID
}
```

### 3.2 Node Kinds

```rust
pub enum NodeKind {
    // Vanilla / external object box
    Obj {
        name: String,
        args: Vec<Token>,
    },
    // Message box
    Msg {
        content: Vec<Vec<Token>>,  // outer = semicolon-separated messages, inner = comma-separated atoms
    },
    // Number box (vanilla)
    FloatAtom {
        width: u32,
        min: f64,
        max: f64,
        label_pos: u8,
        label: Option<String>,
        receive: Option<String>,
        send: Option<String>,
    },
    // Symbol box (vanilla)
    SymbolAtom {
        width: u32,
        label_pos: u8,
        label: Option<String>,
        receive: Option<String>,
        send: Option<String>,
    },
    // Comment
    Text {
        content: String,
    },
    // Named sub-patch (inline pd subpatch) or abstraction (external file)
    SubPatch {
        name: String,
        kind: SubPatchKind,
    },
    // Graph (for arrays)
    Graph,
    // IEM GUI objects
    Gui(GuiObject),
    // Array defined inside a graph sub-canvas
    Array {
        name: String,
        size: u32,
        data_type: String,  // currently always "float"
        flags: u32,
        data: Vec<f64>,
    },
    // Anything else (externals, broken boxes, unknown objects)
    Unknown {
        name: Option<String>,
        args: Vec<Token>,
    },
}

pub enum SubPatchKind {
    // Inline — full content embedded in this AST
    Inline(Box<Canvas>),
    // Abstraction — loader returned None; stored by name only
    Unresolved,
}
```

### 3.3 GUI Object Struct

```rust
pub struct GuiObject {
    pub kind: GuiKind,   // Bang, Toggle, NumberBox, HSlider, VSlider, HRadio, VRadio, Vu, Canvas
    pub size: (u32, u32),
    pub min: f64,
    pub max: f64,
    pub log_scale: bool,
    pub init: bool,
    pub send: Option<String>,
    pub receive: Option<String>,
    pub label: Option<String>,
    pub label_offset: (i32, i32),
    pub font: u8,
    pub font_size: u32,
    pub bg_color: Color,
    pub fg_color: Color,
    pub label_color: Color,
    pub default_value: f64,
    pub extra: GuiExtra,  // kind-specific fields (num_cells for radio, steady_on_click for sliders, etc.)
}

pub struct Color { pub r: u8, pub g: u8, pub b: u8 }
```

### 3.4 Node

```rust
pub struct Node {
    pub id: u32,         // object index within its canvas
    pub x: i32,
    pub y: i32,
    pub kind: NodeKind,
}
```

### 3.5 Connection

```rust
pub struct Connection {
    pub src_node: u32,    // object index
    pub src_outlet: u32,
    pub dst_node: u32,
    pub dst_inlet: u32,
}
```

### 3.6 Canvas

```rust
pub struct Canvas {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub font_size: Option<u32>,   // only Some on root canvas
    pub name: Option<String>,     // only Some on sub-patch canvases
    pub open_on_load: bool,
    pub coords: Option<Coords>,
    pub nodes: Vec<Node>,
    pub connections: Vec<Connection>,
}

pub struct Coords {
    pub x_from: f64,
    pub y_top: f64,
    pub x_to: f64,
    pub y_bottom: f64,
    pub width_px: u32,
    pub height_px: u32,
    pub gop: bool,
    pub x_margin: Option<i32>,
    pub y_margin: Option<i32>,
}
```

### 3.7 Top-level Patch

```rust
pub struct Patch {
    pub root: Canvas,
}

pub struct Warning {
    pub location: Option<u32>,   // object index if relevant
    pub message: String,
}

pub struct ParseResult {
    pub patch: Patch,
    pub warnings: Vec<Warning>,
}
```

---

## 4. Crate Structure

```
pdast/
├── Cargo.toml
├── src/
│   ├── lib.rs           — public API, re-exports, WASM entry points
│   ├── types.rs         — all AST types (Patch, Canvas, Node, NodeKind, etc.)
│   ├── parse/
│   │   ├── mod.rs       — parse_patch(), tokenizer, record splitter
│   │   ├── canvas.rs    — canvas/sub-patch parsing
│   │   ├── obj.rs       — #X obj dispatch (vanilla, GUI, array, sub-patch, unknown)
│   │   ├── gui.rs       — IEM GUI object parsing
│   │   ├── array.rs     — #X array + #A record parsing
│   │   └── message.rs   — message/atom tokenization (dollar, escape handling)
│   ├── emit/
│   │   ├── mod.rs       — emit_patch() → String
│   │   └── canvas.rs    — canvas/node/connection serialization back to .pd
│   └── wasm.rs          — wasm-bindgen entry points + JSON string fallback
├── examples/
│   ├── roundtrip.rs     — parse .pd → emit .pd, diff
│   ├── to_faust.rs      — AST → Faust .dsp code generation
│   └── to_teensy.rs     — AST → Teensy Audio Library C++ code generation
└── tests/
    ├── parse_basic.rs
    ├── parse_gui.rs
    ├── parse_subpatch.rs
    ├── parse_array.rs
    ├── roundtrip.rs
    └── fixtures/        — .pd files for testing
```

---

## 5. Implementation Phases

### Phase 1: Core Parser

1. **Tokenizer / record splitter**: Split file into `#N`/`#X`/`#A` records, handling `\;` and `\,` escapes. Strip CRLF.
2. **Canvas stack**: Implement a parser that maintains a stack of `Canvas` contexts. `#N canvas` pushes a new context; `#X restore` pops it, wraps it in `NodeKind::SubPatch`, and appends to the parent.
3. **Object index tracking**: Maintain a per-canvas counter, incremented for every `#X` except `connect` and `restore`.
4. **`#X obj` dispatch**: Match the object name against:
   - Known IEM GUI names → `NodeKind::Gui`
   - `pd` → inline sub-patch (body follows, look for restore)
   - Everything else → `NodeKind::Obj` (vanilla or external), then call loader
5. **`#X msg`**: Parse message content respecting `,` and `;` separators.
6. **`#X floatatom` / `#X symbolatom`**: Parse all fields.
7. **`#X text`**: Capture comment text.
8. **`#X connect`**: Record connection using current canvas's node indices.
9. **`#X coords`**: Attach `Coords` to current canvas.
10. **`#A`**: Append float values to the most recently defined array node.
11. **Loader callback**: After parsing an `#X obj` that doesn't match known built-ins, call `loader(name)`. If resolved, recursively parse and store as `SubPatch::Inline`. If not, store as `Unknown`.

### Phase 2: Emitter (AST → .pd)

- Walk the AST and produce valid `.pd` text.
- Must regenerate correct object indices and connection records.
- Handle all node kinds including GUI objects and their color encoding.
- Roundtrip test: `parse → emit → parse` should yield identical ASTs.

### Phase 3: WASM Bindings

- Feature flag `wasm` (or `wasm-bindgen` target detection).
- `parse_patch_json(input: &str) -> String` — always available, returns JSON string. Works in non-JS WASM hosts.
- `#[wasm_bindgen] fn parse_patch_js(input: &str) -> JsValue` — returns JS object via `serde-wasm-bindgen`. Only compiled when `wasm-bindgen` feature is enabled.
- Loader callback bridged via `js_sys::Function` for the JS path.

### Phase 4: Examples / Code Generators

#### 4.1 Roundtrip Example

Parse, emit, parse again, assert equality.

#### 4.2 Faust Code Generator

Map PD vanilla DSP objects to Faust library functions:

| PD                         | Faust                       |
| -------------------------- | --------------------------- |
| `osc~`                     | `os.osc`                    |
| `phasor~`                  | `os.phasor`                 |
| `noise~`                   | `no.noise`                  |
| `*~`                       | `*`                         |
| `+~`                       | `+`                         |
| `lop~`                     | `fi.lowpass(1, freq)`       |
| `hip~`                     | `fi.highpass(1, freq)`      |
| `bp~`                      | `fi.bandpass(1, freq, q)`   |
| `dac~`                     | `process` output            |
| `adc~`                     | `process` input             |
| `inlet~`                   | sub-process input           |
| `outlet~`                  | sub-process output          |
| `line~`                    | `ba.line` or `si.smooth`    |
| `delread~` / `delwrite~`   | `de.delay`                  |
| `tabread4~`                | `it.lagrangeN` or `rdtable` |
| `metro`                    | `ba.pulse`                  |
| Control `+`, `-`, `*`, `/` | Faust control math          |

Connections become Faust sequential (`:`) or parallel (`,`) composition. Sub-patches become Faust `component` or inline function.

Known challenges: control-rate vs. audio-rate paths are mixed in PD but distinct in Faust. The generator must separate them.

#### 4.3 Teensy Audio Library Code Generator

Map PD DSP objects to Teensy Audio Library C++ classes:

| PD                      | Teensy                                            |
| ----------------------- | ------------------------------------------------- |
| `osc~` (sine)           | `AudioSynthWaveformSine`                          |
| `osc~` (other wave)     | `AudioSynthWaveform`                              |
| `noise~`                | `AudioSynthNoisePink` / `AudioSynthNoiseWhite`    |
| `*~`                    | `AudioEffectMultiply`                             |
| `dac~`                  | `AudioOutputI2S`                                  |
| `adc~`                  | `AudioInputI2S`                                   |
| `lop~` / `hip~` / `bp~` | `AudioFilterStateVariable` or `AudioFilterBiquad` |
| Mixer (`+~` fan-in)     | `AudioMixer4`                                     |
| Delay                   | `AudioEffectDelay`                                |
| Reverb                  | `AudioEffectReverb`                               |

Generate:

1. Object instantiation declarations (`AudioSynthWaveformSine osc1;`)
2. `AudioConnection` wiring (`AudioConnection c0(osc1, 0, out1, 0);`)
3. `setup()` function with configuration calls
4. `AudioMemory(N)` with estimated buffer count

---

## 6. Public API Surface

```rust
// Parse a .pd patch from a string. The loader is called with an abstraction
// name and should return its content, or None if not available.
pub fn parse_patch<F>(content: &str, loader: F) -> Result<ParseResult, ParseError>
where
    F: Fn(&str) -> Option<String>;

// Convenience: parse without loader (all abstractions become Unknown)
pub fn parse_patch_no_loader(content: &str) -> Result<ParseResult, ParseError>;

// Serialize AST back to .pd patch text
pub fn emit_patch(patch: &Patch) -> String;

// Serialize AST to JSON string
pub fn to_json(patch: &Patch) -> Result<String, serde_json::Error>;

// Deserialize AST from JSON string
pub fn from_json(json: &str) -> Result<Patch, serde_json::Error>;

// WASM (non-JS): parse .pd and return JSON string
#[cfg(target_arch = "wasm32")]
pub fn wasm_parse_to_json(content: &str) -> String;

// WASM (JS host): parse .pd and return JS object
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindgen"))]
#[wasm_bindgen]
pub fn wasm_parse(content: &str) -> JsValue;
```

---

## 7. Cargo.toml Dependencies

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen = { version = "0.2", optional = true }
serde-wasm-bindgen = { version = "0.6", optional = true }
js-sys = { version = "0.3", optional = true }

[features]
default = []
wasm-js = ["wasm-bindgen", "serde-wasm-bindgen", "js-sys"]
```

Build for WASM JS: `wasm-pack build --features wasm-js`
Build for native WASM: `cargo build --target wasm32-wasi`
Build native: `cargo build`

---

## 8. Testing Strategy

- **Unit tests** per parser module with inline fixture strings.
- **Fixture `.pd` files** in `tests/fixtures/` covering:
  - Minimal patch (one object)
  - Fully connected DSP patch
  - Patch with GUI objects (all IEM types)
  - Patch with sub-patches (nested)
  - Patch with abstractions (loader returns content)
  - Patch with arrays
  - Patch with GOP
  - Patch with escaped characters in messages
  - Patch with externals (unknown objects)
- **Roundtrip test**: `emit(parse(file)) == file` (or `parse(emit(parse(file))) == parse(file)` to tolerate whitespace differences).
- **WASM test**: Run via `wasm-pack test --node`.

---

## 9. Out of Scope (for initial version)

- Runtime execution of patches
- Audio signal processing
- Full validation of connection compatibility (signal vs. control)
- Non-float array types (symbol arrays)
- `#X declare` search path handling (the loader callback is the substitute)
- Struct/pointer/scalar data (PD data structures, rarely used in DSP patches)
- `catch~` / `throw~` (global signal buses) — parse as `Obj`, flag in warnings

---

## 10. Key References

- Miller Puckette, "Pure Data" — pd.iscool.net
- Community format spec (2004): `puredata.info/docs/developer/PdFileFormat`
- HVCC source: `github.com/Wasted-Audio/hvcc`
- Faust reference: `faust.grame.fr/doc/manual`
- Teensy Audio Library: `pjrc.com/teensy/td_libs_Audio.html`
- Teensy Audio GUI tool: `pjrc.com/teensy/gui`
- `libpd` (PD as embeddable library): `github.com/libpd/libpd`
