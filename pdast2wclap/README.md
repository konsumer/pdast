# pdast2wclap

Convert a JSON AST (from `pd2ast`) to a self-contained [CLAP](https://github.com/free-audio/clap) plugin, written directly in C — no libpd, no Faust, no runtime patch loading. The whole patch is compiled in at build time; `dac~`/`adc~` become the plugin's audio ports, `notein` becomes real note events, and `receive`/GUI objects become CLAP params.

```
pdast2wclap [OPTIONS] <AST.json | ->

Options:
  -o, --output <FILE>   Write C output to FILE instead of stdout
  -q, --quiet           Suppress warnings
```

## Basic use

```sh
pd2ast my-patch.pd | pdast2wclap -
pdast2wclap my-patch.json
pdast2wclap my-patch.json -o my-patch.c
```

## Full pipeline: patch → JSON → C → wasm

The generated C implements a fixed, documented ABI (`pd_wclap.h`, in this crate) — it is _not_ itself a runnable CLAP plugin. You still need a small "runtime shim" that provides the actual `clap_entry`/plugin surface and calls into that ABI; see [poketrack/plugins/pd2wclap](https://github.com/konsumer/poketrack/tree/main/plugins/pd2wclap) for a complete, working example (shim + build script + demo patches).

```sh
pd2ast my-patch.pd | pdast2wclap - -o my-patch.c
clang --target=wasm32-wasi -mexec-model=reactor \
  -Ipath/to/clap/include -Ipath/to/pd_wclap.h-dir \
  -Wl,--export=clap_entry -Wl,--export=malloc -Wl,--export-table -Wl,--growable-table \
  my-patch.c your-runtime-shim.c -o my-patch.wasm
```

## The `pd_*` ABI (`pd_wclap.h`)

| Symbol                                                                                                     | What it does                                                                                                                                 |
| ---------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `pd_create(sample_rate)` / `pd_destroy(st)`                                                                | Lifecycle                                                                                                                                    |
| `pd_process(st, in_l, in_r, out_l, out_r, nframes)`                                                        | Recomputes the control graph once, then renders `nframes` of audio                                                                           |
| `pd_note_on(st, key, velocity01)` / `pd_note_off(st, key, velocity01)`                                     | Apply a note event (velocity already 0..1) and recompute the control graph                                                                   |
| `pd_control_change(st, controller, value)`                                                                 | MIDI CC — drives any `ctlin` objects (`controller`/`value` 0..127)                                                                           |
| `pd_pitch_bend(st, value)`                                                                                 | Pitch bend — drives any `bendin` objects (0..16383, 8192 == center)                                                                          |
| `pd_touch(st, value)`                                                                                      | Channel pressure — drives any `touchin` objects (0..127)                                                                                     |
| `pd_program_change(st, value)`                                                                             | Program change — drives any `pgmin` objects (0..127)                                                                                         |
| `pd_set_param(st, index, value)` / `pd_get_param(st, index)`                                               | Apply/read a param by its index into `PD_PARAMS[]` (value already scaled into that param's real range, not normalized)                       |
| `PD_NUM_PARAMS`, `PD_PARAMS[]`                                                                             | Param table: name, min, max, default                                                                                                         |
| `PD_HAS_AUDIO_IN`, `PD_HAS_NOTE_IN`, `PD_HAS_CTL_IN`, `PD_HAS_BEND_IN`, `PD_HAS_TOUCH_IN`, `PD_HAS_PGM_IN` | Whether the patch used `adc~` / `notein` / `ctlin` / `bendin` / `touchin` / `pgmin` — a host can skip routing events a patch never asked for |

The MIDI CC/bend/touch/program functions are always safe to call (no-op if the patch has no matching object) — no host is required to call them, and poketrack's own host (see `plugins/pd2wclap` in the poketrack repo) currently doesn't, since it never forwards raw MIDI CC/bend/touch/program-change events to CLAP plugins in the first place. They exist for hosts that do.

A host is expected to split a process block at each event's sample-accurate time offset — call `pd_process()` for the frames up to an event, then `pd_note_on`/`pd_note_off`/`pd_set_param`, then continue — rather than applying all of a block's events up front. `pd_process()` also recomputes the control graph once at the start of every call (in addition to the per-event recomputes above), so time-driven objects (`line~`, delay lines, envelopes) and any pure-control chain downstream of them keep updating even in a block with no events — control-rate updates are quantized to block size, same as most real-time plugin formats.

## What becomes a param

Any `[receive NAME]` / `[r NAME]` / `[value NAME]` object with no matching `[send NAME]` in the same patch, and any GUI object (`hsl`, `vsl`, `nbx`, `tgl`, `bng`, radio) with its **receive** field set, becomes one CLAP param named `NAME` — range and default come from the GUI object's own min/max/init if there is one, otherwise plain 0–1. Deduped by name.

## Built-in object coverage

| Category            | Objects                                                                                                                                                                |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Oscillators         | `osc~`, `phasor~`, `noise~`, `sig~`, `tabosc4~` (linearly interpolated)                                                                                                |
| Audio math          | `+~` `-~` `*~` `/~`, `abs~`, `sqrt~`, `wrap~`, `clip~`                                                                                                                 |
| Filters             | `lop~`, `hip~`, `bp~`, `vcf~`, `biquad~` (constant coefficients), `rzero~`, `rpole~`                                                                                   |
| Envelope / dynamics | `line~`, `vline~`, `env~`, `samphold~`, `snapshot~`, `threshold~`                                                                                                      |
| Delay lines         | `delwrite~`, `delread~`, `vd~` (shared circular buffer, keyed by name)                                                                                                 |
| Arrays / tables     | `tabread~`, `tabread`, `tabwrite~`, and the array data itself (seeded from the saved patch)                                                                            |
| Audio I/O           | `dac~`, `adc~`                                                                                                                                                         |
| MIDI                | `notein`, `ctlin`, `bendin`, `touchin`, `pgmin` (real events, not UI-metadata), `poly` (voice allocator, see below)                                                    |
| Scheduler           | `metro`, `delay`/`del`, `pipe`, `timer` — sample-accurate, see below                                                                                                   |
| Control math        | `+ - * / max min mod pow`, `sin cos atan atan2 abs sqrt log exp wrap clip int`, comparisons (`> < >= <= == != && \|\| !`), `mtof ftom dbtorms rmstodb dbtopow powtodb` |
| Routing             | `moses`, `spigot`, `sel`/`select`, `route`, `change`, `pack`, `unpack`, `swap`, `trigger`/`t`, `line`, `random`, `f`/`float`, `loadbang` (fires once on load)          |
| Buses               | `send` / `s`, `receive` / `r`, `value` (control-rate), `send~` / `receive~`, `throw~` / `catch~` (signal-rate, own namespace)                                          |
| Sub-patch boundary  | `inlet`, `outlet`, `inlet~`, `outlet~` (passthrough — see "Full pipeline" above; every sub-patch/abstraction instance needs these to actually carry values across)    |

Anything else compiles to a harmless zero stub with a `warning:` on stderr rather than failing — the per-object codegen (`wclap_gen.rs`) is built to grow this list. Not yet implemented: `expr`/`expr~`, `cpole~`/`czero~`, multi-message boxes, and symbol/list-typed routing.

### `poly`: build your own polyphony out of PD sub-patches

`[poly <voices=16> <steal=0>]` is a voice allocator, for patches that want real
multi-note polyphony assembled by hand from PD abstractions — the CLAP-level
`pd_note_on`/`pd_note_off` ABI stays a single (pitch, velocity) event stream
(matching how a real MIDI note-on/off arrives), and `poly` fans that one
stream out across N voice slots inside the generated C.

Wire `notein`'s pitch/velocity outlets into `poly`'s two inlets — either as
two separate connections, or the common real-PD shorthand of packing them
first (`[notein] -> [pack f f] -> [poly]`, one wire into `poly`'s inlet 0):
`pack` mirrors every input on its own same-indexed outlet, and when some
node's inlet 0 is fed from a `pack`'s outlet 0 with its inlet 1 otherwise
unconnected, that inlet 1 is auto-filled from the pack's outlet 1 (not
special-cased to `poly` — this applies to any node fed that way). On every
genuine note event (edge-detected the same way `change` is — see below —
so a steady held note doesn't re-trigger every block) it either assigns a
free voice, reuses the voice already holding that exact pitch (retriggering
a held note doesn't steal a second voice), or — once every voice is busy —
either steals whichever voice has been held longest (`steal` nonzero) or
silently drops the extra note (`steal` 0 or omitted, the default).

**This does not use PD's real `poly` contract.** Real PD/Max `poly` emits a
single `(voice#, pitch, velocity)` triple meant to be fanned out downstream
with `[route 1 2 3 ...]` — but this codegen has no list-typed outlets for
`route` to dispatch through (see the symbol/list-typed routing gap above),
so that idiom can't be reproduced. Instead **every voice gets its own
permanently-assigned, continuously-held outlet triple**: for voice `i`
(0-based), outlet `3*i` = pitch, `3*i+1` = gate (1.0 while held, 0.0 while
free), `3*i+2` = velocity. Wire outlet triple `i` straight into instance `i`
of your voice abstraction (an inline sub-patch or a separate `voice.pd`
loaded via `pd2ast`'s abstraction search path — both flatten identically,
see "Full pipeline" above) — no `route`, no dispatch logic needed in the
patch at all. `poly 4 1` therefore has 12 outlets; wire up 4 copies of your
voice patch.

Verified by compiling generated code and driving it through a real note
sequence: round-robin assignment across free voices, retriggering an
already-held pitch reuses its voice instead of stealing a second one,
`steal 1` correctly steals the *oldest*-held voice when full, and a
note-off correctly finds and releases the right voice even after it was
reassigned by a steal.

**You can still write the standard `poly` → `[pack f f f]` → `[route 1 2 3 ...]`
idiom if you want the patch to also work unmodified in real PD.** The
codegen recognizes that exact shape — a `route` whose targets are `1..N`
consecutively, fed (directly or through one `pack` hop) from outlet 0 of a
`poly` node whose voice count matches `N` — and compiles it straight
through `poly`'s own per-voice outlets above, bypassing `route`/`pack`'s
literal semantics entirely (a `warning:` on stderr confirms the rewrite
fired). This isn't optional polish: `route`'s literal semantics are
architecturally impossible to honor correctly here regardless of how
`pack`/`route` are implemented, since real PD's version depends on a
discrete message *latching* at the receiving object until the next message
arrives on that specific wire — a concept this continuously-recomputed
control graph has no equivalent of. With only one shared `poly` output
feeding a live `route`, the instant a second voice fires, every other
voice's `route` row re-evaluates against the new key and drops to zero,
cutting it off — not a bug to fix downstream, an inherent consequence of
"every control node is a stateless formula over current inputs," which
this whole codegen is built on (see the module doc in `wclap_gen.rs`).
`pack`/`route` still get emitted as harmless dead code alongside the
rewrite; only the target-count mismatch case (or any other route/pack
shape) falls through to their literal, single-connection-wins behavior.

Message boxes (`[1 10(`-style) are also **not reactive**: a message box compiles to a fixed constant taken from its literal contents at build time, not a value that's re-emitted when something bangs it — wiring two different message boxes into the same downstream inlet (a common attack/release idiom: `[sel 0] -> [1 10(` / `[0 200(` -> one `vline~` inlet) silently keeps only the first-declared one and drops the other, since `input_expr` resolution only honors one incoming connection per inlet. Drive dynamic targets through control-math objects (comparisons, `moses`, `spigot`, `f`) instead of through message boxes.

### `metro`/`delay`/`pipe`/`timer`: real, sample-accurate scheduling

These aren't tilde objects, but their timing math runs every sample (forced into the signal domain internally) rather than only at control-recompute time, so they're accurate regardless of block size — verified by compiling generated code and running it: a `metro 5` at 48kHz produces pulses at exactly 240-sample intervals, and `delay 5` fires exactly once, ~240 samples after being triggered. None of them have a real discrete "bang" input in this continuous model, so they're driven by _edges_ on inlet 0 instead:

- `metro` runs (re-fires periodically) while inlet 0 is nonzero — pair it with a toggle or a mapped param, not a one-shot bang.
- `delay`/`del` (re)arms on a rising edge (0 → nonzero) of inlet 0.
- `pipe` (re)arms whenever inlet 0's value _changes_, and forwards that value (not just a bang) once the delay elapses.
- `timer` continuously reports elapsed ms since inlet 0's last rising edge, rather than only on a second bang.

### Discrete message/bang semantics: what's approximated

pdast2wclap's control graph is _continuous_ — every control node's value is recomputed on each event/block, not driven by discrete one-shot messages. This is a better fit for most of vanilla PD than pdast2faust's per-sample recompute (see below), but a few real objects are fundamentally message/bang-driven and only get an honest approximation here:

| PD object        | Here                                             | What's lost                                                                                 |
| ---------------- | ------------------------------------------------ | ------------------------------------------------------------------------------------------- |
| `change`         | Always mirrors input                             | Real `change` only outputs when the value differs                                           |
| `snapshot~`      | Continuously mirrors the signal's current sample | Real `snapshot~` captures only on a bang                                                    |
| `threshold~`     | Continuous 0/1 gate                              | Real `threshold~` bangs once on crossing                                                    |
| `tabwrite~`      | Continuous circular recording                    | Real `tabwrite~` is a bang-armed one-shot recording                                         |
| `random`         | Regenerated every recompute                      | Real `random` only regenerates on a bang                                                    |
| `line` / `line~` / `vline~` | Exponential ramp toward the input, fixed time | Real `line`/`vline~` take a `(target, time[, delay])` message pair per ramp — no per-call time override, and `vline~`'s multi-segment/delay syntax isn't modeled at all |
| `loadbang`       | Fires once on the first control recompute        | Matches real PD closely — the only difference is the exact recompute that counts as "load"  |

## Fixes vs. `pdast2faust`

This backend fixes three gaps present in `pdast2faust`: `$1`/`$2` creation-args are actually substituted when inlining sub-patches/abstractions (not always `0`), an inlined sub-patch's internal nodes are properly hoisted into the parent scope instead of producing dangling references, and feedback loops (cycles) are well-defined by construction — every node's output is addressed through persistent state rather than a fresh local, so a cyclic read naturally gets last-pass's value instead of needing explicit back-edge detection.

## Control-rate semantics

Unlike `pdast2faust` (which recomputes every control object on every audio sample — see the semantic caveats above), `pdast2wclap` recomputes the control graph only at actual event boundaries (a note, MIDI CC/bend/touch/program event, or a mapped param changing) plus once per `pd_process()` call, matching PD's own message-passing behaviour much more closely. A control value feeding a signal-rate inlet is sample-and-held between updates, same as real PD.
