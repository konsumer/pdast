// PD: notein [channel]  — MIDI note input
// Outlets: 3 (pitch, velocity, channel)
// Faust standard MIDI mapping: freq from note, gate from velocity > 0.
import("stdfaust.lib");
pdobj = ba.hz2midikey(hslider("freq[midi:freq]", 440, 20, 20000, 1)),
        hslider("gain[midi:gain]", 0.5, 0, 1, 0.01),
        checkbox("gate[midi:gate]");
