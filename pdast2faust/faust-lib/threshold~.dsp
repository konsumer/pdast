// PD: threshold~ <lo> <hi>  — schmitt trigger on audio signal
// Inlets: 3 (audio~, lo, hi), Outlets: 2 (rising edge, falling edge)
// args: lo(float)=0, hi(float)=1
import("stdfaust.lib");
pdobj(lo, hi) = ef.gate_mono(lo, hi);
