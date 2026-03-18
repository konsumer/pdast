// PD: phasor~ [freq=440]
// Inlets: 1 (frequency), Outlets: 1 (audio 0..1 sawtooth)
// args: freq(float)=440
import("stdfaust.lib");
pdobj = os.phasor(1);
