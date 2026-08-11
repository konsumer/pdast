# pdast2mozzi

Convert a JSON AST (from `pd2ast`) to a self-contained [Mozzi](https://sensorium.github.io/Mozzi/)
Arduino sketch (`.ino`), written in C++ — no libpd, no runtime patch
loading. The whole patch is compiled in, the same philosophy as
[`pdast2wclap`](../pdast2wclap/README.md): `osc~`/`phasor~`/`noise~` become
real Mozzi unit generators, `dac~` becomes the sketch's `updateAudio()`
return value, and MIDI objects become hook functions your own MIDI library
calls into.

```
pdast2mozzi [OPTIONS] <AST.json | ->

Options:
  -o, --output <FILE>   Write .ino output to FILE instead of stdout
  -q, --quiet           Suppress warnings
```

## Basic use

```sh
pd2ast my-patch.pd | pdast2mozzi - -o my-patch.ino
```

Open `my-patch.ino` in the Arduino IDE (or `arduino-cli`), install the
[Mozzi library](https://github.com/sensorium/Mozzi) (2.x), pick a board,
and upload. `MOZZI_CONTROL_RATE` is set to 64 Hz by default — edit the
`#define` at the top of the generated file if your patch needs faster or
slower control-rate response.

### PlatformIO

[`example/`](example/) is a ready-to-build PlatformIO project — it pulls in
Mozzi via `lib_deps` (no manual Library Manager step) and has a
`generate.sh` that regenerates `src/main.ino` from a `.pd` patch. `cd`
into it and run `pio run -t upload`; see its own README for details.

## Why this maps onto Mozzi so directly

Mozzi's own execution model is a close match for PD's: `updateAudio()` is
called once per audio sample (PD's signal domain), and `updateControl()` is
called on a fixed periodic tick, `MOZZI_CONTROL_RATE` (PD's own control
block-rate, conceptually). This generator leans into that:

- **Signal-rate (tilde) objects** are pulled continuously and recomputed
  every `updateAudio()` call.
- **Control-rate objects** are a genuine **message-passing graph** (not a
  recomputed dataflow expression) — same reasoning as `pdast2wclap`: a
  recompute pass can't express "only the outlet that fired propagates"
  (`route`/`select`), "cold inlets hold their last value" (the classic
  `[f]`/`[+ 1]` counter), or right-to-left `trigger` ordering. Every message
  node gets a `pd_nX_inK()` inlet handler and `pd_nX_outJ()` outlet fan-out
  function.
- **Time-driven objects** (`metro`, `delay`/`del`, `pipe`, `timer`) use a
  real Mozzi `EventDelay` per node, checked once per control tick — so their
  resolution is bounded by `MOZZI_CONTROL_RATE`, not sample-accurate. This
  is an intentional trade for staying idiomatic Mozzi rather than hand-
  rolling a sample-accurate scheduler.

There's no `PdState` struct/pointer indirection like `pdast2wclap` has —
a Mozzi sketch only ever has one instance, so every node's persistent value
is just a plain global variable (`pd_nID`).

## Numeric model

Audio-rate values are `int32_t`, following Mozzi's own native table-
amplitude convention (roughly **-128..127 per "unit"**, matching an 8-bit
wavetable), not PD's normalized -1..1 signal convention. This matters for:

- `*~`/`+~`/`-~`/`/~` with a **scalar** creation arg (e.g. `[*~ 0.5]`) — the
  scalar itself is scale-invariant, so these work exactly as in PD.
- `clip~ lo hi` — its creation-arg bounds are written in PD's -1..1
  convention, so they're automatically rescaled by ×128 to match this
  generator's amplitude convention.
- `line~`/`vline~` targets are **not** rescaled (a ramp target might mean an
  amplitude, a frequency, or something else entirely — there's no single
  correct scale factor to guess). If you're using `line~` as a 0..1
  amplitude envelope multiplying an oscillator, aim the ramp at ±128
  (matching the oscillator's own output range) instead of ±1.
- `phasor~` is the one exception to the ±128 convention: it stays a plain
  **0.0..~1.0 float**, matching real PD exactly, because it's a unipolar
  ramp/index signal, not an audio-amplitude one — idioms like
  `[phasor~ 5] -> [*~ 20] -> [+~ 440]` (an LFO sweep) depend on that native
  range. Multiply it by 128 yourself if you want it in the ±128 convention
  for direct use as an audio signal.

Control-rate values are `float`. The final mix at the `dac~` sink is
clamped to ±256 (`MonoOutput`/`StereoOutput::fromNBit(9, ...)`) — a modest
2x headroom over one oscillator's raw ±128 output, so a couple of summed
voices won't immediately clip. Push it further if you're mixing many
voices, or lower it back to 8-bit if you're only after one oscillator's
worth of range.

## Integration hooks (`pd_*`)

Mozzi has no MIDI input and no host to bind params to — both are exposed as
plain functions your own sketch code calls:

```cpp
// From your MIDI library's callback (e.g. Arduino MIDI Library):
MIDI.setHandleNoteOn([](byte ch, byte note, byte vel){ pd_note_on(note, vel); });
MIDI.setHandleNoteOff([](byte ch, byte note, byte vel){ pd_note_off(note, vel); });
MIDI.setHandleControlChange([](byte ch, byte cc, byte val){ pd_control_change(cc, val); });

// From updateControl(), to wire a potentiometer to a param:
pd_set_param(0, mozziAnalogRead(A0) / 1023.0f * (PD_PARAMS[0].max - PD_PARAMS[0].min) + PD_PARAMS[0].min);
```

| Function | Fires when patch uses |
| --- | --- |
| `pd_note_on(byte note, byte velocity)` / `pd_note_off(...)` | `notein` |
| `pd_control_change(byte controller, byte value)` | `ctlin` |
| `pd_pitch_bend(int value)` | `bendin` |
| `pd_touch(byte value)` | `touchin` |
| `pd_program_change(byte value)` | `pgmin` |
| `pd_set_param(int index, float value)` / `pd_get_param(int index)` | any param (see below) |

Calling a hook the patch doesn't use is always safe (no-op).

## What becomes a param

Any `[receive NAME]`/`[r NAME]`/`[value NAME]` with no matching `[send NAME]`
in the same patch, and any GUI object (`hsl`, `vsl`, `nbx`, `tgl`, `bng`,
radio) with its **receive** field set, becomes one `PD_PARAMS[]` entry
(name/min/max/default), settable via `pd_set_param()`/readable via
`pd_get_param()` — deduped by name, same contract as `pdast2wclap`.

## Built-in object coverage

| Category | Objects | Mozzi mapping |
| --- | --- | --- |
| Oscillators | `osc~` | `Oscil<SIN2048_NUM_CELLS, MOZZI_AUDIO_RATE>` |
| | `phasor~` | hand-rolled 32-bit phase accumulator (not a table — see below) |
| | `noise~` | self-contained xorshift PRNG (not Mozzi's own noise API — see below) |
| | `sig~` | constant/control value cast to signal |
| Audio math | `+~ -~ *~ /~ abs~ clip~` | plain C++ arithmetic (float intermediate, `int32_t` result) |
| Filters | `lop~ hip~` | hand-rolled one-pole IIR |
| Envelope/ramp | `line~ vline~` | Mozzi `Line<int32_t>` |
| Delay | `delwrite~ delread~ vd~` | shared `AudioDelayFeedback<N>` keyed by name (feedback disabled — see below) |
| Audio I/O | `dac~` | `updateAudio()` return value (mono or stereo, by inlet count) |
| | `adc~` | not implemented — zero stub + warning |
| MIDI | `notein ctlin bendin touchin pgmin` | `pd_*` hook functions (see above) |
| Scheduler | `metro delay/del pipe timer` | `EventDelay`-based, `MOZZI_CONTROL_RATE` resolution |
| Control math | `+ - * / mod pow max min sin cos atan atan2 abs sqrt log exp wrap clip int` + comparisons (`> < >= <= == != && \|\| !`) | direct C++ / `<math.h>` |
| | `mtof ftom` | Mozzi's real `mtof()`/`ftom()` |
| | `dbtorms rmstodb dbtopow powtodb` | manual math (no Mozzi equivalent) |
| Routing | `moses spigot sel/select route change pack/unpack swap trigger/t random f/float loadbang` | message-graph handlers |
| Buses | `send/receive/value` (control), `send~/receive~` (signal) | shared global, by name |
| Sub-patch boundary | `inlet outlet inlet~ outlet~` | resolved away entirely by flattening (see below) |
| GUI | `hsl vsl nbx tgl bng hradio vradio` w/ `receive` | → param (see above) |

Every sub-patch and abstraction instance is fully flattened (graph-spliced,
with `$1`/`$2` creation-arg substitution) before codegen — same approach as
`pdast2wclap`, so `inlet~`/`outlet~` boundaries and nested abstractions just
disappear into the top-level graph rather than needing their own runtime
representation.

Anything else compiles to a harmless zero stub with a `warning:` on stderr
rather than failing — the per-object codegen (`mozzi_gen.rs`) is built to
grow this list.

### Implementation notes worth knowing about

- **`phasor~`** is a hand-rolled phase accumulator, not `Oscil` over a
  generated ramp table. Mozzi's wavetables are expected to live in
  board-specific storage (`PROGMEM` on AVR) via macros this generator isn't
  confident it can reproduce correctly for every target — a hand-rolled
  accumulator sidesteps that risk entirely and is how `Oscil` works
  internally anyway, just without the table indirection.
- **`noise~`** and **`random`** use a small self-contained PRNG rather than
  Mozzi's own `rand()`/`mozzi_rand.h` (whose exact signature wasn't verified
  against source at generation time) — same reasoning as `phasor~` above,
  and the same technique `pdast2wclap` already uses for `random`.
- **`delwrite~`/`delread~`/`vd~`**: the read tap time is fixed to the
  `delwrite~`'s own `maxms` creation argument — `delread~`'s own ms argument
  and `vd~`'s dynamic modulation input aren't applied. `AudioDelayFeedback`'s
  feedback level is set to 0 (plain delay, not the class's built-in
  feedback) since PD's `delwrite~`/`delread~` pair has no feedback of its
  own — wire your own feedback patch cord if you want one. Buffer size is a
  compile-time expression resolved against the real `MOZZI_AUDIO_RATE`
  macro at *sketch* compile time (not baked in as a number here, since that
  depends on your board), clamped to 2–2048 samples to stay AVR-SRAM-
  friendly.
- **Verified against the real Mozzi library**, not just eyeballed: both
  [`example/`](example/)'s vibrato patch and a heavier stress patch
  (`notein`/`mtof`/`osc~`/`line~`/`metro`/message boxes/`sel`/`dac~`) build
  clean via PlatformIO (`sensorium/Mozzi@2.0.4`) targeting a real Arduino
  Uno (`atmelavr`/AVR toolchain) — no warnings, ~15% flash / ~55% RAM for
  the stress patch. That's still not the same as running on physical
  hardware, though — do that before trusting a generated sketch in a real
  build.

## Explicitly out of scope for v1

Same spirit as `pdast2faust`/`pdast2wclap`'s own "known issues" sections —
documented gaps, not silent wrong behavior:

- `expr`/`expr~` (PD's C-style expression language isn't parsed).
- `tabread4~`/`tabosc4~` and arbitrary user wavetables — PD's saved `array`
  data isn't embedded as a custom Mozzi table yet.
- `poly` (N-voice allocator) — see `pdast2wclap`'s README for what a full
  implementation looks like; not ported here yet.
- Cross-canvas `send`/`receive` (different canvases/abstractions aren't
  wired together).
- Symbol-typed messages/routing — this codegen is numeric throughout, same
  limitation `pdast2wclap` has; a symbol atom degrades to `0.0`.
- Exotic filters: `bp~ vcf~ biquad~ rzero~ rpole~`.
- Real audio input (`adc~`) — board-specific, not implemented.
