// PD: rpole~ <coeff>  — one-pole filter (IIR)
// Inlets: 2 (audio~, coeff), Outlets: 1 (audio~)
// args: coeff(float)=0
pdobj(a) = loop ~ _
  with { loop(s) = _ + s * a; };
