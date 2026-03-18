// PD: biquad~ b0 b1 b2 a1 a2  — direct-form biquad filter
// Inlets: 6 (audio~, b0, b1, b2, a1, a2), Outlets: 1 (audio~)
// args: b0 b1 b2 a1 a2 (all float)
import("stdfaust.lib");
pdobj(b0, b1, b2, a1, a2) = fi.tf2(b0, b1, b2, a1, a2);
