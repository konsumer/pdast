// PD: vcf~ [freq=0] [Q=1]
// Voltage-controlled filter (bandpass)
// Inlets: 3 (audio in, center freq, Q/resonance), Outlets: 2 (bandpass, lowpass)
// args: freq(float)=0, Q(float)=1
import("stdfaust.lib");
pdobj(freq, Q) = fi.resonbp(max(1.0, freq), max(0.01, Q), 1.0) <: _,_;
