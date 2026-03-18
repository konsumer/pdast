// PD: bang  — output a bang on receive or button click
// Mapped to a Faust button UI element producing a rising-edge pulse.
import("stdfaust.lib");
risingEdge = _ <: _, mem : >;
pdobj = button("bang") : risingEdge;
