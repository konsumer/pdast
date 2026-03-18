// PD: line~
// Linear ramp generator (control->audio)
// Inlets: 1 (target value + time), Outlets: 1
import("stdfaust.lib");
// Approximation: one-pole smoother (not exact PD semantics)
pdobj = si.smooth(ba.tau2pole(0.005));
