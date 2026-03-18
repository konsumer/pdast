// PD: delread~ <name> [delay_ms=0]
// Read from a named delay line.
// Inlets: 1 (delay time ms), Outlets: 1 (delayed audio)
// args: name(symbol), delay_ms(float)=0
// NOTE: handled specially by the generator
import("stdfaust.lib");
pdobj(maxdel, del) = de.delay(int(maxdel * ma.SR / 1000.0), int(del * ma.SR / 1000.0));
