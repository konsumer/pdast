// PD: value <name>  — shared named float (global variable)
// In Faust there are no runtime globals; the generator substitutes all
// [value X] / [send X] / [receive X] sharing the same name with a common
// nentry UI element at the top level.
// This stub is used only when the name can't be resolved by the generator.
import("stdfaust.lib");
pdobj(init) = nentry("value", init, -1e9, 1e9, 0.001);
