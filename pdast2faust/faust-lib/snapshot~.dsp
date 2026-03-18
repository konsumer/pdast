// PD: snapshot~  — sample an audio signal on a bang, output as control float
// Inlets: 2 (audio~, bang), Outlets: 1 (control float)
// The bang inlet is treated as a rising-edge trigger.
import("stdfaust.lib");
risingEdge = _ <: _, mem : >;
pdobj(bang_sig) = ba.sAndH(risingEdge(bang_sig));
