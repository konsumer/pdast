// PD: delwrite~ <name> <max_delay_ms>
// Write to a named delay line.
// Inlets: 1 (audio in), Outlets: 0
// args: name(symbol), max_delay_ms(float)
// NOTE: handled specially by the generator (paired with delread~)
pdobj = _;
