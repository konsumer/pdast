// PD: float [init=0]  — store a number; output on bang, update from right inlet
// Approximation: sample-and-hold. The "bang" inlet is the second inlet;
// use a rising-edge trigger to sample the right-inlet value.
// In a connected patch the generator wires this appropriately.
// Standalone: just passes the value through (always-on equivalent).
import("stdfaust.lib");
pdobj(init) = ba.sAndH(1, init);
