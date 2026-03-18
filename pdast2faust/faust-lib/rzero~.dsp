// PD: rzero~ <coeff>  — one-zero filter (FIR)
// Inlets: 2 (audio~, coeff), Outlets: 1 (audio~)
// args: coeff(float)=0
pdobj(b) = _ <: _, (mem : *(b)) : -;
