// PD: rmstodb  — RMS amplitude to dB
// Inlets: 1 (linear amplitude ≥ 0), Outlets: 1 (dB)
pdobj = max(1e-20) : log10 : *(20.0);
