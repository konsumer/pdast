// PD: env~ [n_points]  — RMS amplitude envelope follower
// Inlets: 1 (audio~), Outlets: 1 (RMS amplitude, control rate)
import("stdfaust.lib");
pdobj(n) = an.amp_follower_ud(ba.samp2ms(max(64, n)) / 1000.0,
                               ba.samp2ms(max(64, n)) / 1000.0);
