// PD: wrap~
// Wrap signal to [0,1)
// Inlets: 1, Outlets: 1
pdobj = x : x - floor(x) with { x = _; };
