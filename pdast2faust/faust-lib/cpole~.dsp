// PD: cpole~  — complex one-pole filter
// Inlets: 4 (real-in~, imag-in~, real-coeff~, imag-coeff~), Outlets: 2
import("stdfaust.lib");
pdobj(xr, xi, ar, ai) =
  (xr + loop_r ~ _) , (xi + loop_i ~ _)
  with {
    loop_r(sr) = sr * ar - (xi + sr * ai) * ai;
    loop_i(si) = si * ai + (xr + si * ar) * ar;
  };
