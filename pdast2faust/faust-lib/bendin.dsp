// PD: bendin [channel]  — MIDI pitch bend input (-8192 to 8191)
import("stdfaust.lib");
pdobj = hslider("bend[midi:pitchwheel]", 0, -8192, 8191, 1);
