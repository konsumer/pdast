// PD: pack  — combine inlets into a single list message
// Faust has no typed messages; numeric fields become parallel signals.
// Approximation: pass all inlets through as parallel signals.
pdobj = _,_;
