// PD: trigger / t  — on input, fire outlets right-to-left in sequence
// Faust has no sequential outlet firing; all outputs are simultaneous.
// Approximation: pass the input through to all outlets in parallel.
// WARNING: outlet ordering semantics are NOT preserved.
pdobj = _ <: !,_;
