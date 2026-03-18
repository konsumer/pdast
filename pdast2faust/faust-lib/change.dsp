// PD: change  — pass value only when it differs from the previous value
// Inlets: 1, Outlets: 1
// Approximation: Faust compares to previous sample.
pdobj = _ <: _, (_' != _) : *;
