// PD: dbtorms  — dB to RMS amplitude (0 dB = amplitude 1.0)
// Inlets: 1 (dB), Outlets: 1 (linear amplitude)
pdobj = _ / 20.0 : pow(10.0);
