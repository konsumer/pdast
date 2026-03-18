// PD: clip <lo> <hi>  — hard clip (control rate)
// Inlets: 3 (signal, lo, hi), Outlets: 1
// args: lo(float)=-1, hi(float)=1
pdobj(lo, hi) = max(lo, min(hi, _));
