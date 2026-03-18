// PD: metro <interval_ms>  — periodic trigger pulse
// Inlets: 2 (on/off, interval_ms), Outlets: 1 (pulse)
// Approximation: block-aligned; slight drift vs. PD wall-clock metro.
// args: interval_ms(float)=500
import("stdfaust.lib");
pdobj(ms) = ba.pulse(max(1, int(ba.ms2samp(ms))));
