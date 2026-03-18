// PD: line  — linear ramp: given (target, time_ms) pair, ramp to target
// Inlets: 1 (target value; second inlet overrides time), Outlets: 1
// Approximation: exponential smoothing (si.smooth) rather than linear.
// For a true linear ramp you would need to track the target externally.
import("stdfaust.lib");
pdobj(ms) = si.smooth(ba.tau2pole(max(0.0001, ms / 1000.0)));
