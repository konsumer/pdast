// PD: sel / select <value>  — output bang (1.0) when input == value, else 0
// Inlets: 2 (value, target), Outlets: 1+ (one per target value)
// Single-target version — generator handles multi-target as multiple outputs.
// args: target(float)=0
pdobj(target) = ==(target) : float;
