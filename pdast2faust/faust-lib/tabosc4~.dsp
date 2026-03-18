// PD: tabosc4~ <arrayname>  — 4-point interpolating wavetable oscillator
// Inlets: 1 (freq Hz), Outlets: 1 (audio~)
// NOTE: requires array data; the generator emits a sine oscillator stub.
// Replace with rdtable + ba.midikey2hz for accurate wavetable playback.
import("stdfaust.lib");
pdobj = os.osc;  // sine stub — replace with rdtable when array is available
