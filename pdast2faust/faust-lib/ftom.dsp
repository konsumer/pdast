// PD: ftom  — frequency (Hz) to MIDI note number
// Inlets: 1 (Hz), Outlets: 1 (MIDI note, float)
import("stdfaust.lib");
pdobj = ba.hz2midikey;
