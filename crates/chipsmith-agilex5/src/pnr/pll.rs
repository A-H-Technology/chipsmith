//! An IOPLL as design-file lines (agilex5-re README (v); its
//! `tools/pll/iopll.py` and `compute.py`): the common template
//! (`db/sm4revb/io/iopll.tsv`) plus what follows from the frequencies: the
//! N, M and C counters, C and N as hi = ceil(x/2), lo = floor(x/2) with the
//! odd-duty bit for odd x, M as the 9-bit `gdiv` hi field, the reference mux
//! input from the reference pin, output enables and charge-pump trims by PFD
//! frequency. The PLL is the one in the reference ball's hviowr instance;
//! only bank-half 1 reference pins (pad `HVIO_B1_2_<k>`, the DE25-Nano's
//! PLLREFCLK pins) are modelled.

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};

const P: &str = "xpllwrap_top/xpllwrap_bf/xiopllwrap_top/xbf/";
const VCO_MAX: u64 = 3200;
const VCO_MIN: u64 = 600;
const PFD_MIN: u64 = 10;
const C_MAX: u64 = 512;

/// Charge-pump current trims and lock filter by PFD frequency (MHz), from
/// pl and pa: (cpi_trim[4:0], cpp_trim[4:0], cr_lockf_early[3:0]).
const TRIMS: [(u64, [&str; 3]); 3] = [
    (50, ["00110", "01001", "1000"]),
    (25, ["00011", "00011", "1000"]),
    (10, ["00010", "00010", "1011"]),
];

/// A non-negative rational in lowest terms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Q {
    num: u64,
    den: u64,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Q {
    fn new(num: u64, den: u64) -> Q {
        let g = gcd(num, den).max(1);
        Q {
            num: num / g,
            den: den / g,
        }
    }
    fn khz(k: u64) -> Q {
        Q::new(k, 1000)
    }
    fn mul(self, o: Q) -> Q {
        Q::new(self.num * o.num, self.den * o.den)
    }
    fn div(self, o: Q) -> Q {
        Q::new(self.num * o.den, self.den * o.num)
    }
    fn int(self) -> Option<u64> {
        (self.den == 1).then_some(self.num)
    }
}

/// N, M and each output's C: the rule the altera_iopll IP follows, fitted on
/// agilex5-re's training sweep. The VCO candidates are the multiples of the
/// outputs' least common multiple, from 3200 MHz down to 600 MHz; for each,
/// the smallest N with a PFD frequency (ref / N) of at least 10 MHz that
/// makes M = VCO / PFD an integer; the first candidate that has one wins.
/// C_i = VCO / out_i (at most 512).
fn compute(reference: Q, outs: &[Q]) -> Option<(u64, u64, Vec<u64>)> {
    // The LCM of rationals: lcm(numerators) / gcd(denominators).
    let (mut num, mut den) = (1u64, 0u64);
    for o in outs {
        num = num * o.num / gcd(num, o.num);
        den = gcd(den, o.den);
    }
    let step = Q::new(num, den);
    let mut k = VCO_MAX * step.den / step.num;
    while k >= 1 && step.num * k >= VCO_MIN * step.den {
        let vco = step.mul(Q::new(k, 1));
        let cs: Option<Vec<u64>> = outs
            .iter()
            .map(|&o| vco.div(o).int().filter(|&c| c <= C_MAX))
            .collect();
        if let Some(cs) = cs {
            for n in 1..=reference.num / (PFD_MIN * reference.den) {
                if let Some(m) = vco.mul(Q::new(n, 1)).div(reference).int() {
                    return Some((n, m, cs));
                }
            }
        }
        k -= 1;
    }
    None
}

/// The lines of the PLL whose reference enters at `ball`, `ref_khz` in and
/// `out_khz` out.
pub fn lines(db: &str, ball: &str, ref_khz: u64, out_khz: &[u64]) -> Result<String> {
    let pins =
        std::fs::read_to_string(format!("{db}/io/pins.tsv")).context("reading io/pins.tsv")?;
    let (mut uid, mut pad) = ("", "");
    for l in pins.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f[0] == ball && f.len() > 2 {
            (uid, pad) = (f[1], f[2]);
        }
    }
    let half = match pad
        .strip_prefix("HVIO_B1_2_")
        .and_then(|r| r.split_once('_'))
    {
        Some(("0", _)) => 0,
        Some(("1", _)) => 1,
        _ => bail!("{ball}: not a bank-half 1 PLL reference pin (HVIO_B1_2_0/1)"),
    };
    let reference = Q::khz(ref_khz);
    let outs: Vec<Q> = out_khz.iter().map(|&k| Q::khz(k)).collect();
    let (n, m, cs) = compute(reference, &outs)
        .with_context(|| format!("PLL {ref_khz} kHz to {out_khz:?} kHz: no exact solution"))?;
    let pfd = (ref_khz as f64 / 1000.0 / n as f64).round_ties_even() as u64;
    let [cpi, cpp, lockf] = TRIMS
        .iter()
        .find(|(f, _)| *f == pfd)
        .map(|(_, t)| *t)
        .with_context(|| format!("PFD {pfd} MHz: no charge-pump trims known (only 10, 25, 50)"))?;
    let used = cs.len();
    let c = format!("{P}xiopll_custom/");
    let mut out = String::new();
    let template =
        std::fs::read_to_string(format!("{db}/io/iopll.tsv")).context("reading io/iopll.tsv")?;
    for l in template.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = l.split('\t').collect();
        // Counters 2..6 idle bypassed; a used one isn't.
        let counter = f[2]
            .split_once("xccnt_iopll")
            .and_then(|(_, r)| r.split_once("/dprio_c_m_cnt_bypass_en"))
            .and_then(|(k, _)| k.parse::<usize>().ok());
        if counter.is_some_and(|k| k < used) {
            continue;
        }
        writeln!(out, "{}\t{uid}\t{}", f[0], f[2..].join("\t"))?;
    }
    let attr = |out: &mut String, path: &str, v: &str| writeln!(out, "attr\t{uid}\t{path}\t{v}");
    let direct = |out: &mut String, path: &str, v: u64, w: usize| {
        writeln!(out, "direct\t{uid}\t{path}\t{v:0w$b}")
    };
    let bits = |out: &mut String, path: &str, s: &str| -> std::fmt::Result {
        for (i, ch) in s.chars().rev().enumerate() {
            writeln!(out, "bit\t{uid}\t{path}[{i}]\t{ch}")?;
        }
        Ok(())
    };
    attr(
        &mut out,
        "xpllwrap_top/xpllwrap_10to1_mux/hviowr_pllwrap_clk_mux_sel",
        &format!("PLL_CLKMUX_IOCLKIN_{}", 2 + half),
    )?;
    for k in 0..used.max(2) {
        let cv = cs.get(k).copied().unwrap_or(510);
        let cnt = format!("{c}xccnt_iopll{k}/");
        direct(
            &mut out,
            &format!("{cnt}dprio_c_m_cnt_hi_div_user"),
            cv.div_ceil(2),
            8,
        )?;
        direct(
            &mut out,
            &format!("{cnt}dprio_c_m_cnt_lo_div_user"),
            cv / 2,
            8,
        )?;
        if cv % 2 == 1 {
            attr(
                &mut out,
                &format!("{cnt}dprio_c_m_cnt_even_duty_en"),
                "DPRIO_CNT_EVEN_DUTY_ENABLED",
            )?;
        }
        if (1..used).contains(&k) {
            attr(
                &mut out,
                &format!("{c}xpllcoutbuf{k}/cnt_out_en"),
                "CNT_OUTPUT_ENABLE",
            )?;
        }
    }
    direct(
        &mut out,
        &format!("{c}xiopll_core/gdiv/dprio_c_m_cnt_hi_div_user"),
        m,
        9,
    )?;
    if n > 1 {
        let nc = format!("{c}xiopll_core/xncnt_iopll/");
        attr(
            &mut out,
            &format!("{nc}pll_n_cnt_bypass_en"),
            "PLL_N_CNT_DIV_ENABLED",
        )?;
        direct(&mut out, &format!("{nc}pll_n_cnt_hi_div"), n.div_ceil(2), 8)?;
        direct(&mut out, &format!("{nc}pll_n_cnt_lo_div"), n / 2, 8)?;
        if n % 2 == 1 {
            attr(
                &mut out,
                &format!("{nc}pll_n_cnt_odd_div_duty_en"),
                "PLL_N_CNT_EVEN_DUTY_ENABLED",
            )?;
        }
    }
    bits(
        &mut out,
        &format!("{c}xiopll_core/ipllljtopana/xchgpmplf/cpi_trim"),
        cpi,
    )?;
    bits(
        &mut out,
        &format!("{c}xiopll_core/ipllljtopana/xchgpmplf/cpp_trim"),
        cpp,
    )?;
    bits(&mut out, &format!("{P}xfpll_ctrl/cr_lockf_early"), lockf)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// compute.py's answers (agilex5-re `tools/pll/compute.py`).
    #[test]
    fn counters_as_the_ip_picks_them() {
        let c = |r: u64, o: &[u64]| {
            compute(Q::khz(r), &o.iter().map(|&k| Q::khz(k)).collect::<Vec<_>>())
        };
        assert_eq!(c(50_000, &[100_000]), Some((1, 64, vec![32])));
        assert_eq!(c(50_000, &[33_333]), None);
        assert_eq!(c(50_000, &[125_000]), Some((2, 125, vec![25])));
        assert_eq!(c(50_000, &[25_000, 100_000]), Some((1, 64, vec![128, 32])));
        assert_eq!(c(25_000, &[200_000]), Some((1, 128, vec![16])));
        assert_eq!(c(50_000, &[7_000]), Some((1, 63, vec![450])));
        assert_eq!(c(100_000, &[300_000, 150_000]), Some((1, 30, vec![10, 20])));
        assert_eq!(c(50_000, &[1_000]), None);
        assert_eq!(c(50_000, &[3_000_000]), Some((1, 60, vec![1])));
    }
}
