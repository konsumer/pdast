# pdast2mozzi + PlatformIO example

A minimal, ready-to-build PlatformIO project showing how a `pdast2mozzi`-
generated sketch fits together: PlatformIO handles the board target and the
Mozzi library dependency (no manual Arduino Library Manager step), and
`generate.sh` handles turning a `.pd` patch into `src/main.ino`.

```
example/
├── platformio.ini   ← board target + `lib_deps = sensorium/Mozzi@^2.0.0`
├── patch.pd          ← the source patch (edit this)
├── generate.sh        ← regenerates src/main.ino from patch.pd
└── src/main.ino      ← generated output (checked in, so `pio run` works
                         immediately without building the Rust CLI tools)
```

## The patch

`patch.pd` is a small vibrato tone: a 5 Hz `phasor~` LFO sweeps a 440 Hz
`osc~` by ±20 Hz, through a gain stage into a stereo `dac~`. It needs no
buttons, pots, or MIDI to be audible — plug in a speaker/headphones on the
board's audio-out pin (pin 9 on an Uno, by default) and go.

```
phasor~ 5 → *~ 20 → +~ 440 → osc~ → *~ 0.5 → dac~
```

## Build & upload

```sh
pio run              # build
pio run -t upload    # build + flash
```

The default environment (`[env:uno]`) targets a classic Arduino Uno.
`platformio.ini` has commented-out `esp32`/`teensy40` environments too —
uncomment one and run `pio run -e esp32` (etc.) if that's your board. The
generated code itself is board-agnostic; only the `platformio.ini`
environment needs to change.

## Editing the patch

1. Open `patch.pd` in Pure Data (or edit it by hand — it's plain text) and
   change it however you like.
2. Regenerate the sketch:
   ```sh
   ./generate.sh
   ```
3. `pio run -t upload` again.

`generate.sh` looks for `pd2ast`/`pdast2mozzi` on your `PATH` first
(`cargo install --path ../../pd2ast && cargo install --path ..` from this
directory installs both); if it doesn't find them, it falls back to
`cargo run` against the workspace so this works right out of a fresh clone.

## Adding MIDI or potentiometer control

The generated `src/main.ino` owns `setup()`/`loop()`/`updateControl()`
itself (see the crate's own [README](../README.md) for why), so wiring up
a `pd_note_on()`/`pd_set_param()` call from a MIDI library or an
`mozziAnalogRead()`-based pot means adding a call inside the generated
`updateControl()` after regenerating — regeneration will overwrite that
edit, so keep it as a small diff you reapply, rather than hand-editing
`patch.pd`-driven logic in place. See the crate README's "Integration
hooks" section for the exact function signatures.
