// PD: ctlin [controller] [channel]  — MIDI control change input
// Outlets: 3 (value 0-127, controller, channel)
// Approximation: single nentry for the CC value; controller/channel are fixed.
import("stdfaust.lib");
pdobj = nentry("ctrl[midi:ctrl 1]", 0, 0, 127, 1);
