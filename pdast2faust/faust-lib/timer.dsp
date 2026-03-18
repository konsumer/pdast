// PD: timer  — measure elapsed time (ms) between two bangs
// Inlets: 2 (reset bang, read bang), Outlets: 1 (elapsed ms)
// Approximation: counts samples between rising edges, converts to ms.
// The reset inlet resets the counter; the read inlet outputs the count.
import("stdfaust.lib");
risingEdge = _ <: _, mem : >;
pdobj(reset_trig, read_trig) =
  loop ~ _
  with {
    loop(count) =
      select2(risingEdge(reset_trig), count + 1, 0);
  } : *(ba.sAndH(risingEdge(read_trig), 1000.0 / ma.SR));
