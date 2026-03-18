// PD: samphold~  — sample and hold at audio rate
// Inlets: 2 (audio~, control~), Outlets: 1 (audio~)
// Holds input signal when control rises above previous value.
import("stdfaust.lib");
risingEdge = _ <: _, mem : >;
pdobj(ctrl) = ba.sAndH(risingEdge(ctrl));
