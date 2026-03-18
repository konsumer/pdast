// PD: delay <delay_ms>  — delay a bang/message by N ms
// Inlets: 2 (trigger, delay_ms), Outlets: 1 (delayed trigger)
// Approximation: delays a trigger pulse by N samples using de.delay.
// Signal timing is exact at sample level; wall-clock drift same as metro.
import("stdfaust.lib");
pdobj(ms) = de.delay(ma.SR, ba.ms2samp(max(0, ms)));
