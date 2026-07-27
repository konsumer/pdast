# pdast2faust

Convert a JSON AST (from `pd2ast`) to a [Faust](https://faust.grame.fr/) `.dsp` file.

```
pdast2faust [OPTIONS] <AST.json | ->

Options:
  -L, --lib <DIR>       Extra library directory for object templates (repeatable)
  -o, --output <FILE>   Write Faust code to FILE instead of stdout
  -q, --quiet           Suppress warnings
```

## Basic use

```sh
pd2ast my-patch.pd | pdast2faust -
pdast2faust my-patch.json
pdast2faust my-patch.json -o my-patch.dsp
```

## Full pipeline: patch → JSON → Faust

```sh
pd2ast my-patch.pd | pdast2faust - -o my-patch.dsp
faust -o my-patch.cpp my-patch.dsp          # compile to C++
faust2jaqt my-patch.dsp                     # build a JACK standalone app
faust2lv2 my-patch.dsp                      # build an LV2 plugin
```

## Object templates (lib dirs)

Each supported PD object has a `.dsp` template file named `<object-name>.dsp`. A built-in library covers common vanilla PD DSP objects. You can add your own for externals or custom abstractions.

```sh
# Use a custom lib dir to support fiddle~ (or any other object)
pdast2faust my-patch.json -L ~/my-faust-lib

# Multiple lib dirs, searched in order before the built-in library
pdast2faust my-patch.json -L ./project-lib -L ~/global-lib
```

### Writing a template file

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

### Built-in object coverage

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

## Code generation model

The generator produces a Faust `with { }` block where each PD node becomes a named binding (`n0`, `n1`, …). This means:

- Fan-out connections (one outlet → many inlets) are handled without duplicating computation.
- Control-rate and audio-rate nodes are mixed freely in the same graph.
- Every node is computed on every sample (always-on), which is the correct Faust model.

## Semantic caveats

| PD concept                      | Faust approximation                 | What's lost                                               |
| ------------------------------- | ----------------------------------- | --------------------------------------------------------- |
| `metro` wall-clock timing       | Block-aligned `ba.pulse`            | Slight drift; a 1ms metro fires every block, not every ms |
| `float` / `int` storage         | `ba.sAndH` always-on                | Bang → output becomes always-outputting                   |
| `trigger` / `t` ordering        | Simultaneous outputs                | Right-to-left outlet firing order is not preserved        |
| `send` / `receive`              | Direct binding wire (within canvas) | Cross-patch buses not supported                           |
| `route` (by type/symbol)        | Not supported                       | Symbol routing has no Faust equivalent                    |
| `pack` / `unpack` (mixed types) | Numeric fields only                 | Symbol fields dropped                                     |
| `expr` / `expr~`                | Passthrough stub                    | PD's C-style expression language needs manual conversion  |

## Limitations of Faust output

- `tabread4~` and arrays generate placeholder stubs — wire up `rdtable` manually for wavetable playback.
- Feedback loops through control objects (`float` driving itself via `+`) produce a one-sample Faust feedback delay (`~`), which is correct for audio but may produce subtle ordering differences for control logic.
- `delwrite~`/`delread~` pairs are treated as independent nodes. Pair them manually by sharing a `de.delay` instance.
- Sub-patches are inlined as expressions, but creation-argument substitution (`$1`, `$2` → values) is not yet performed.
