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

| Symbol                                                                 | What it does                                                                                                           |
| ---------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `pd_create(sample_rate)` / `pd_destroy(st)`                            | Lifecycle                                                                                                              |
| `pd_process(st, in_l, in_r, out_l, out_r, nframes)`                    | Render `nframes` of audio; does **not** advance the control graph                                                      |
| `pd_note_on(st, key, velocity01)` / `pd_note_off(st, key, velocity01)` | Apply a note event (velocity already 0..1) and recompute the control graph                                             |
| `pd_set_param(st, index, value)` / `pd_get_param(st, index)`           | Apply/read a param by its index into `PD_PARAMS[]` (value already scaled into that param's real range, not normalized) |
| `PD_NUM_PARAMS`, `PD_PARAMS[]`                                         | Param table: name, min, max, default                                                                                   |
| `PD_HAS_AUDIO_IN`, `PD_HAS_NOTE_IN`                                    | Whether the patch used `adc~` / any MIDI-in object                                                                     |

A host is expected to split a process block at each event's sample-accurate time offset — call `pd_process()` for the frames up to an event, then `pd_note_on`/`pd_note_off`/`pd_set_param`, then continue — rather than applying all of a block's events up front.

## What becomes a param

Any `[receive NAME]` / `[r NAME]` / `[value NAME]` object with no matching `[send NAME]` in the same patch, and any GUI object (`hsl`, `vsl`, `nbx`, `tgl`, `bng`, radio) with its **receive** field set, becomes one CLAP param named `NAME` — range and default come from the GUI object's own min/max/init if there is one, otherwise plain 0–1. Deduped by name.

## Built-in object coverage

| Category     | Objects                                                 |
| ------------ | ------------------------------------------------------- |
| Oscillators  | `osc~`, `phasor~`, `noise~`, `sig~`                     |
| Audio math   | `+~` `-~` `*~` `/~`                                     |
| Filters      | `lop~`, `hip~`, `vcf~` (resonant bandpass)              |
| Audio I/O    | `dac~`, `adc~`                                          |
| MIDI         | `notein` (real note events, not UI-metadata)            |
| Control math | `+` `-` `*` `/` `max` `min` `mod` `pow`, `mtof`, `ftom` |
| Buses        | `send` / `s`, `receive` / `r`, `value`                  |

Anything else compiles to a harmless zero stub with a `warning:` on stderr rather than failing — the per-object codegen (`wclap_gen.rs`) is built to grow this list. Not yet implemented: arrays/tables (`tabread~` etc.), `ctlin`/`bendin`, `trigger`/`route`/`select`/`pack`/`unpack`.

## Fixes vs. `pdast2faust`

This backend fixes three gaps present in `pdast2faust`: `$1`/`$2` creation-args are actually substituted when inlining sub-patches/abstractions (not always `0`), an inlined sub-patch's internal nodes are properly hoisted into the parent scope instead of producing dangling references, and feedback loops (cycles) are well-defined by construction — every node's output is addressed through persistent state rather than a fresh local, so a cyclic read naturally gets last-pass's value instead of needing explicit back-edge detection.

## Control-rate semantics

Unlike `pdast2faust` (which recomputes every control object on every audio sample — see the semantic caveats above), `pdast2wclap` recomputes the control graph only at actual event boundaries (a note or a mapped param changing), matching PD's own message-passing behaviour much more closely. A control value feeding a signal-rate inlet is sample-and-held between updates, same as real PD.
