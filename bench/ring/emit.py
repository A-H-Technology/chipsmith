#!/usr/bin/env python3
"""Write timing.rs's cell-delay block from the ring and path fits.

    bench/ring/emit.py <silicon.json> <constants.json> <guardband ps> [timing.rs]

<silicon.json> is bench/ring/derive.py's (this chip at room temperature),
<constants.json> bench/ring/path_fit.py's (each class's slow-corner scale
and the register constants, fitted to quartus_sta's path totals on
chipsmith's placements); derive.model combines them. The guardband is added to every
path start (each register-to-register path has exactly one), so the model
can be made more pessimistic than the fits by a fixed amount per path.
Replaces the lines between `// BEGIN bench/ring/emit.py` and `// END`.
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from derive import model  # noqa: E402

DEFAULT = "crates/chipsmith-route/src/timing.rs"


def main():
    si = json.load(open(sys.argv[1]))
    c = json.load(open(sys.argv[2]))
    g = float(sys.argv[3])
    path = sys.argv[4] if len(sys.argv) > 4 else DEFAULT
    kl, kc, km = c["k_lut"], c["k_carry"], c["k_mult"]
    cells = model(si, kl, kc, km)
    r = lambda v: f"{round(v):.1f}"
    lut = ", ".join(r(v) for v in cells["lut"])
    block = f"""// BEGIN bench/ring/emit.py
// Silicon: ring oscillators on the DE2's EP2C35 at room temperature
// (bench/ring/measure.sh, fit.py, derive.py), scaled to the slow corner by
// one factor per class: LUT {kl:.3f}, carry chain {kc:.3f}, multiplier
// {km:.3f}, with the model's own LOCAL_LINE (229 ps) taken off a LUT arc.
// Fitted: the factors and the register constants, least squares over
// quartus_sta's path totals on chipsmith's placements (path_fit.py;
// aggregate, against the model's own routing).
// Guardband: {g:.0f} ps on every path start (emit.py).
/// LUT input to COMBOUT through each physical input A to D, the LE input's
/// interconnect included. Silicon.
const LUT_PIN: [Ps; 4] = [{lut}];
/// A carry chain's head: input A or B to COUT. Silicon (the head and the
/// tail always come together on a path; the tail is taken as input C's
/// arc, which is where CIN enters the LUT).
const CARRY_HEAD: [Ps; 2] = [{r(cells["head_a"])}, {r(cells["head_b"])}];
/// CIN to COMBOUT (a chain's sum out). Input C's arc, see CARRY_HEAD.
const CIN_COMBOUT: Ps = {r(cells["cin_combout"])};
/// CIN to COUT inside a LAB half. Silicon.
const CIN_COUT: Ps = {r(cells["cin_cout"])};
/// What a carry costs on top where it crosses the LAB's half-way buffer
/// (LE 7 to 8) and where it leaves the LAB (LE 15 to the next LAB's 0).
/// Silicon.
const CARRY_LCB: Ps = {r(cells["lcb"])};
const CARRY_LAB: Ps = {r(cells["lab"])};
/// An unregistered multiplier, data in to product out: a full 18 x 18
/// block and one whose operands fit a nine-bit row, each the slowest
/// product bit measured. Silicon.
const MULT_COMB: Ps = {r(cells["mult_comb"])};
const MULT9_COMB: Ps = {r(cells["mult9_comb"])};
/// LE register clock to REGOUT. Fitted, plus the guardband.
const TCO: Ps = {r(c["tco LE"] + g)};
/// What each register input needs before the capturing clock edge, its
/// interconnect included (SDATA's physical input C's too). Fitted; DATAIN
/// is held (only TCO + TSU is identifiable) and the others are relative
/// to it.
const TSU_DATAIN: Ps = {r(c["tsu DATAIN"])};
const TSU_SDATA: Ps = {r(c["tsu SDATA"])};
const TSU_ENA: Ps = {r(c["tsu ENA"])};
const TSU_SCLR: Ps = {r(c["tsu SCLR"])};
const TSU_SLOAD: Ps = {r(c["tsu SLOAD"])};
/// M4K read clock to data out without and with the output register, and
/// its input registers' setup. Fitted (the launch ones plus the
/// guardband); they take up the block's clock arriving later than an LE
/// register's.
const M4K_TCO: Ps = {r(c["tco M4K"] + g)};
const M4K_OUTREG_TCO: Ps = {r(c["tco M4K outreg"] + g)};
const M4K_TSU: Ps = {r(c["tsu M4K"])};
/// Cyclone II I/O-cell registers, for a register-to-register path that
/// starts or ends in a pin's I/O cell. Fitted (the launch plus the
/// guardband).
const IOE_TCO: Ps = {r(c["tco IOE"] + g)};
const IOE_TSU: Ps = {r(c["tsu IOE"])};
// END bench/ring/emit.py"""
    text = open(path).read()
    new, n = re.subn(r"// BEGIN bench/ring/emit\.py.*?// END bench/ring/emit\.py", lambda _: block, text, flags=re.S)
    assert n == 1, "no emit.py block in timing.rs"
    open(path, "w").write(new)


if __name__ == "__main__":
    main()
