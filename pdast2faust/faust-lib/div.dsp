// PD: / [init=1]  — control-rate divide (protected against div-by-zero)
// Inlets: 2 (left, right), Outlets: 1
// args: right_operand(float)=1
pdobj(r) = /(max(1e-38, abs(r)) * (r >= 0 : select2(_, -1, 1)));
