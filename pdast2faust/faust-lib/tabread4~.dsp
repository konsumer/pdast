// PD: tabread4~ <arrayname>
// 4-point interpolating table read (audio rate)
// Inlets: 1 (index 0..size-1), Outlets: 1
// NOTE: array must be declared; handled specially by generator
import("stdfaust.lib");
// Placeholder: identity (generator replaces with rdtable reference)
pdobj = _;
