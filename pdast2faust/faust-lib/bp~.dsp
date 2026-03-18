// PD: bp~ [freq=0] [Q=1]
// Bandpass filter (2-pole)
// Inlets: 3 (audio in, center freq, Q), Outlets: 1
// args: freq(float)=0, Q(float)=1
import("stdfaust.lib");
pdobj(freq, Q) = fi.resonbp(max(1.0, freq), max(0.01, Q), 1.0);
