// PD: vd~ <name>
// Variable-delay read (interpolated)
// Inlets: 1 (delay time ms), Outlets: 1
// NOTE: handled specially by the generator
import("stdfaust.lib");
pdobj(maxdel, del) = de.fdelay(int(maxdel * ma.SR / 1000.0), del * ma.SR / 1000.0);
