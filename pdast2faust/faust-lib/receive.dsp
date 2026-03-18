// PD: receive / r <name>  — receive from a named send bus
// The generator resolves send/receive pairs into shared nentry UI elements.
// This stub is used only when the name can't be resolved.
import("stdfaust.lib");
pdobj(name) = nentry("receive", 0, -1e9, 1e9, 0.001);
