// PD: moses <n>  — split: left outlet if < n, right outlet if >= n
// Inlets: 2 (value, threshold), Outlets: 2
// args: n(float)=0
pdobj(n) = _ <: (*(< n)), (*(>= n));
