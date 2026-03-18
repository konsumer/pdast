// PD: clip~ <lo> <hi>
// Hard clip signal
// Inlets: 3 (signal, lo, hi), Outlets: 1
// args: lo(float)=-1, hi(float)=1
pdobj(lo, hi) = max(lo, min(hi, _));
