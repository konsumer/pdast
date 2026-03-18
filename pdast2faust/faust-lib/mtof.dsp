// PD: mtof  — MIDI note number to frequency (Hz)
// Inlets: 1 (MIDI note 0-127), Outlets: 1 (Hz)
import("stdfaust.lib");
pdobj = ba.midikey2hz;
