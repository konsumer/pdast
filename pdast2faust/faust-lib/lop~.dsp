// PD: lop~ [freq=0]
// One-pole lowpass filter
// Inlets: 2 (audio in, cutoff freq), Outlets: 1
// args: freq(float)=0
import("stdfaust.lib");
pdobj(freq) = fi.lowpass(1, max(1.0, freq));
