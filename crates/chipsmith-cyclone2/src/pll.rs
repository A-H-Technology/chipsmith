//! A PLL as a design asks for it (`CY2_PLL`, `synth/cells.v`): an input
//! clock period and, per output, a multiply/divide ratio, phase shift and
//! duty cycle, altpll's way of saying it. `solve` finds the counters that
//! make exactly those frequencies (ADR-0008), and the shifts as near as
//! the VCO's eighth-period taps get, the way Quartus picks them.
//!
//! The limits are Quartus' own for the EP2C35 (its `cycloneii_pll` atoms
//! carry them, cyclone2-re's PLL fuzz designs): the VCO between 500 and
//! 1000 MHz (600 with the post-scale), the phase detector between 10 and
//! 402.5 MHz; n is 1-4, m and each output counter 1-32 (a high and a low
//! count of 1-16 each), a preset (`initial`) of 1-16 VCO cycles.

use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};

use crate::bitstream::pll::{Config, Divider, Feedback, Loop, Output};

/// VCO and phase-detector limits, in hertz.
const VCO_MIN_HZ: f64 = 500e6;
/// With the post-scale the atom's VCO range is 300-500 MHz after it
/// (`vco_min` 2000 ps, `vco_max` 3333 ps): the VCO itself at 600-1000 MHz.
/// Quartus took 10 MHz at -2 ns from a 600 MHz VCO (83 ps off) over an
/// exact 500 MHz one.
const VCO_DIV2_MIN_HZ: f64 = 600e6;
const VCO_MAX_HZ: f64 = 1000e6;
const PFD_MIN_HZ: f64 = 10e6;
const PFD_MAX_HZ: f64 = 402.5e6;
/// The VCO frequency Quartus' choices sit closest to (cyclone2-re README,
/// "PLLs": 800 MHz for 50, 100 and 400 MHz out of 50 MHz).
const VCO_PREFERRED_HZ: f64 = 800e6;

/// One output as asked for: `f_in * multiply / divide`, shifted by
/// `phase_ps` (negative: earlier than the input clock), high for
/// `duty_percent` of the period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRequest {
    pub multiply: u32,
    pub divide: u32,
    pub phase_ps: i32,
    pub duty_percent: u32,
}

/// A `CY2_PLL` cell's parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The input clock's period (altpll's `inclk0_input_frequency`).
    pub inclk_period_ps: u32,
    /// c0, c1, c2; none for an output nothing uses.
    pub outputs: [Option<OutputRequest>; 3],
    pub feedback: Feedback,
}

/// The counters for a request, and what they make.
#[derive(Debug, Clone, PartialEq)]
pub struct Solved {
    pub config: Config,
    /// Each output's frequency, in hertz.
    pub hz: [Option<f64>; 3],
    /// Each output's shift as made, in picoseconds: the request rounded to
    /// the nearest eighth of a VCO period.
    pub phase_ps: [Option<f64>; 3],
}

/// A number parameter as the netlist JSON carries it: a binary string
/// (yosys' integers) or decimal text.
fn int(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.len() >= 8 && v.chars().all(|c| c == '0' || c == '1') {
        u64::from_str_radix(v, 2).ok()
    } else {
        v.trim_matches('"').parse().ok()
    }
}

/// A signed number parameter: yosys writes a negative integer as its 32-bit
/// two's complement, decimal text keeps its sign.
fn signed(v: &str) -> Option<i32> {
    let t = v.trim();
    if t.len() >= 8 && t.chars().all(|c| c == '0' || c == '1') {
        int(t).map(|u| u as u32 as i32)
    } else {
        t.trim_matches('"').parse().ok()
    }
}

impl Request {
    /// From a `CY2_PLL` cell: its parameters, and which of its outputs
    /// (`CLK0`-`CLK2`) drive something.
    pub fn from_params(params: &HashMap<String, String>, used: [bool; 3]) -> Result<Request> {
        let num = |k: &str, default: u64| -> Result<u32> {
            match params.get(k) {
                Some(v) => {
                    Ok(int(v).with_context(|| format!("{k} = {v:?} is not a number"))? as u32)
                }
                None => Ok(default as u32),
            }
        };
        let mut outputs = [None; 3];
        for (k, o) in outputs.iter_mut().enumerate() {
            if !used[k] {
                continue;
            }
            let key = format!("CLK{k}_PHASE_SHIFT");
            let phase_ps = match params.get(&key) {
                Some(v) => signed(v).with_context(|| format!("{key} = {v:?} is not a number"))?,
                None => 0,
            };
            *o = Some(OutputRequest {
                multiply: num(&format!("CLK{k}_MULTIPLY_BY"), 1)?,
                divide: num(&format!("CLK{k}_DIVIDE_BY"), 1)?,
                phase_ps,
                duty_percent: num(&format!("CLK{k}_DUTY_CYCLE"), 50)?,
            });
        }
        let mode = params
            .get("OPERATION_MODE")
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_else(|| "NORMAL".into());
        let feedback = match mode.as_str() {
            "NORMAL" => Feedback::Normal,
            "NO_COMPENSATION" => Feedback::NoCompensation,
            m => bail!("CY2_PLL OPERATION_MODE {m:?}: NORMAL or NO_COMPENSATION"),
        };
        Ok(Request {
            inclk_period_ps: num("INCLK0_INPUT_FREQUENCY", 20_000)?,
            outputs,
            feedback,
        })
    }

    /// The input clock frequency in hertz.
    pub fn inclk_hz(&self) -> f64 {
        1e12 / f64::from(self.inclk_period_ps)
    }

    /// The counters: n, m (times a VCO post-scale of 1 or 2) and one
    /// counter per output such that every output's frequency is exactly
    /// `f_in * multiply / divide`, the VCO and the phase detector inside
    /// their limits and every preset at most 16 cycles. Of those, as
    /// Quartus picks: the VCO post-scale of 1, the shifts nearest the
    /// request (the sum of each output's error), a VCO nearest 800 MHz,
    /// then the smallest n.
    ///
    /// A shift is a whole number of eighths of the counters' clock period
    /// (`f_in * m / n`), rounded to the nearest: whole periods into a
    /// counter's preset, the rest onto a VCO tap (fuzz p5, p16). A negative
    /// shift delays the feedback counter by the most negative one and every
    /// output with it, so the outputs lead the input clock (cyclone2-re
    /// specimens: -3 ns at a 1000 MHz VCO is `m_initial` 4; -1.5 ns on c2
    /// next to c0 at 0 is `m_initial` 2, `m_ph` 4 and c0 at preset 2, tap
    /// 4). Quartus doesn't fold a shift into the period: -12 ns at 100 MHz
    /// is 12 VCO cycles of feedback delay.
    pub fn solve(&self) -> Result<Solved> {
        ensure!(
            self.inclk_period_ps > 0,
            "CY2_PLL INCLK0_INPUT_FREQUENCY is 0"
        );
        let used: Vec<(usize, OutputRequest)> = self
            .outputs
            .iter()
            .enumerate()
            .filter_map(|(k, o)| Some((k, (*o)?)))
            .collect();
        ensure!(!used.is_empty(), "CY2_PLL with no output in use");
        for (k, o) in &used {
            ensure!(
                o.multiply > 0 && o.divide > 0,
                "CY2_PLL CLK{k}: multiply and divide must be positive"
            );
            ensure!(
                (1..100).contains(&o.duty_percent),
                "CY2_PLL CLK{k}: duty cycle {}%",
                o.duty_percent
            );
        }
        let fin = self.inclk_hz();
        struct Candidate {
            k: u32,
            n: u32,
            m: u32,
            counts: [u32; 3],
            /// Feedback delay and each output's delay, in eighths of a
            /// counter-clock period.
            feedback: u32,
            taps: [u32; 3],
        }
        // (post-scale, phase error in thousandths of a ps, distance from
        // 800 MHz, n)
        let mut best: Option<((u32, u64, u64, u32), Candidate)> = None;
        // The lowest post-scale with counters for the frequencies, shifts
        // aside: Quartus stays there and clamps a shift its presets can't
        // hold (warning 15559: -40 ns at 100 MHz became -30 ns at post-scale
        // 1 though post-scale 2 makes -40 ns exactly).
        let mut first_k = None;
        for k in 1..=2u32 {
            let vco_min = if k == 1 { VCO_MIN_HZ } else { VCO_DIV2_MIN_HZ };
            for n in 1..=4u32 {
                let pfd = fin / f64::from(n);
                if !(PFD_MIN_HZ..=PFD_MAX_HZ).contains(&pfd) {
                    continue;
                }
                // m >= 4: the lock window below that was never seen.
                for m in 4..=32u32 {
                    let vco = pfd * f64::from(m * k);
                    if !(vco_min..=VCO_MAX_HZ).contains(&vco) {
                        continue;
                    }
                    // Counter k of output o: c = m * divide / (n * multiply),
                    // an integer.
                    let mut counts = [0u32; 3];
                    let ok = used.iter().all(|&(i, o)| {
                        let num = u64::from(m) * u64::from(o.divide);
                        let den = u64::from(n) * u64::from(o.multiply);
                        if num % den != 0 {
                            return false;
                        }
                        counts[i] = (num / den) as u32;
                        (1..=32).contains(&counts[i])
                    });
                    if !ok {
                        continue;
                    }
                    first_k.get_or_insert(k);
                    let eighth_ps = 1e12 / (pfd * f64::from(m)) / 8.0;
                    let mut want = [0i64; 3];
                    let mut err = 0.0;
                    for &(i, o) in &used {
                        let t = (f64::from(o.phase_ps) / eighth_ps).round();
                        want[i] = t as i64;
                        err += (t * eighth_ps - f64::from(o.phase_ps)).abs();
                    }
                    let feedback = used
                        .iter()
                        .map(|&(i, _)| -want[i])
                        .max()
                        .unwrap_or(0)
                        .max(0);
                    let mut taps = [0u32; 3];
                    for &(i, _) in &used {
                        taps[i] = (want[i] + feedback) as u32;
                    }
                    // A preset counts 1-16 cycles, with a tap on the 16th
                    // (Quartus took a 700 MHz VCO for -20 ns at 100 MHz, 15
                    // cycles, where 1000 MHz needs 21; +18875 ps is preset
                    // 16, tap 1). Quartus would also weigh a VCO that clamps
                    // by its error; one that clamps never wins here, so a
                    // request near the limit could get another VCO than
                    // Quartus' (never seen).
                    if feedback / 8 >= 16 || used.iter().any(|&(i, _)| taps[i] / 8 >= 16) {
                        continue;
                    }
                    let score = (
                        k,
                        (err * 1000.0).round() as u64,
                        (vco - VCO_PREFERRED_HZ).abs() as u64,
                        n,
                    );
                    if best.as_ref().is_none_or(|(b, _)| score < *b) {
                        best = Some((
                            score,
                            Candidate {
                                k,
                                n,
                                m,
                                counts,
                                feedback: feedback as u32,
                                taps,
                            },
                        ));
                    }
                }
            }
        }
        let best = best.filter(|(_, c)| Some(c.k) == first_k);
        let Some((
            _,
            Candidate {
                k,
                n,
                m,
                counts,
                feedback,
                taps,
            },
        )) = best
        else {
            let outs = used
                .iter()
                .map(|(i, o)| {
                    format!(
                        "CLK{i} = {:.3} MHz at {} ps",
                        fin * f64::from(o.multiply) / f64::from(o.divide) / 1e6,
                        o.phase_ps
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            if first_k.is_some() {
                bail!(
                    "CY2_PLL: {outs} from {:.3} MHz: those shifts need more than 16 VCO cycles of delay at every VCO that makes these frequencies (Quartus clamps them, warning 15559)",
                    fin / 1e6
                )
            }
            bail!(
                "CY2_PLL: no counters make {outs} from {:.3} MHz with the VCO at 500-1000 MHz",
                fin / 1e6
            )
        };
        // The counters run on the VCO divided by the post-scale.
        let fvco = fin * f64::from(m) / f64::from(n);
        let eighth_ps = 1e12 / fvco / 8.0;
        let mut outputs = [None; 3];
        let mut hz = [None; 3];
        let mut phase_ps = [None; 3];
        for &(i, o) in &used {
            let count = counts[i];
            let divider = if count == 1 {
                None
            } else {
                let high = (count * o.duty_percent).div_ceil(100).clamp(1, count - 1);
                let odd = count % 2 == 1 && o.duty_percent == 50;
                Some(
                    Divider::new(high as u8, (count - high) as u8, odd)
                        .with_context(|| format!("CY2_PLL CLK{i}"))?,
                )
            };
            outputs[i] = Some(Output {
                divider,
                initial: (taps[i] / 8 + 1) as u8,
                phase: (taps[i] % 8) as u8,
            });
            hz[i] = Some(fvco / f64::from(count));
            phase_ps[i] = Some((f64::from(taps[i]) - f64::from(feedback)) * eighth_ps);
        }
        Ok(Solved {
            config: Config {
                inclk_period_ps: self.inclk_period_ps,
                n: n as u8,
                m: Divider::new(m.div_ceil(2) as u8, (m / 2) as u8, m % 2 == 1)?,
                m_initial: (feedback / 8 + 1) as u8,
                m_phase: (feedback % 8) as u8,
                vco_div2: k == 2,
                outputs,
                feedback: self.feedback,
                r#loop: Loop::for_m(m * k),
            },
            hz,
            phase_ps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(outs: &[(u32, u32)]) -> Request {
        let mut outputs = [None; 3];
        for (k, &(multiply, divide)) in outs.iter().enumerate() {
            outputs[k] = Some(OutputRequest {
                multiply,
                divide,
                phase_ps: 0,
                duty_percent: 50,
            });
        }
        Request {
            inclk_period_ps: 20_000,
            outputs,
            feedback: Feedback::Normal,
        }
    }

    fn mc(s: &Solved) -> (u32, Vec<u32>) {
        let c = &s.config;
        (
            c.m.count() * if c.vco_div2 { 2 } else { 1 },
            c.outputs
                .iter()
                .flatten()
                .map(|o| o.divider.map_or(1, Divider::count))
                .collect(),
        )
    }

    /// The counters Quartus picked for the same requests (cyclone2-re's
    /// PLL fuzz designs p1, p2, p4, p14, p15).
    #[test]
    fn quartus_picks_the_same_counters() {
        assert_eq!(mc(&req(&[(2, 1)]).solve().unwrap()), (16, vec![8]));
        assert_eq!(
            mc(&req(&[(3, 2), (1, 2), (4, 5)]).solve().unwrap()),
            (12, vec![8, 24, 15])
        );
        assert_eq!(mc(&req(&[(7, 5)]).solve().unwrap()), (14, vec![10]));
        assert_eq!(mc(&req(&[(8, 1)]).solve().unwrap()), (16, vec![2]));
        // 10 MHz: the VCO at 600 MHz, post-scaled by 2.
        let s = req(&[(1, 5)]).solve().unwrap();
        assert!(s.config.vco_div2);
        assert_eq!(mc(&s), (12, vec![30]));
        assert_eq!(s.hz[0], Some(10e6));
    }

    /// p2's c2: 15, odd, 8 high and 7 low; p6: 25% of 16 is 4 high.
    #[test]
    fn duty_cycle_and_odd_counts() {
        let s = req(&[(3, 2), (1, 2), (4, 5)]).solve().unwrap();
        let c2 = s.config.outputs[2].unwrap().divider.unwrap();
        assert_eq!((c2.high(), c2.low(), c2.odd()), (8, 7, true));
        let mut r = req(&[(1, 1)]);
        r.outputs[0].as_mut().unwrap().duty_percent = 25;
        let c0 = r.solve().unwrap().config.outputs[0]
            .unwrap()
            .divider
            .unwrap();
        assert_eq!((c0.high(), c0.low(), c0.odd()), (4, 12, false));
    }

    /// p5: 2500 ps at an 800 MHz VCO is two whole periods: preset 3.
    #[test]
    fn phase_shift() {
        let mut r = req(&[(2, 1)]);
        r.outputs[0].as_mut().unwrap().phase_ps = 2500;
        let o = r.solve().unwrap().config.outputs[0].unwrap();
        assert_eq!((o.initial, o.phase), (3, 0));
    }

    type Out = Option<(u32, u32, i32)>;
    /// (n, m, post-scale, m_initial, m_ph) and each counter's (count,
    /// initial, ph).
    type Fit = ((u8, u32, u32, u8, u8), [Option<(u32, u8, u8)>; 3]);

    /// altpll designs Quartus fit (cyclone2-re's `tools/gen_clk.py` kind
    /// `pll` from CLOCK_50; their post-fit `cycloneii_pll` atoms): the
    /// outputs as (multiply, divide, phase ps), and the counters it picked.
    /// Negative shifts (q1, q2, r4, r7, r8, r11-r13, s7, s9, t17, t19,
    /// t20) delay the feedback counter; every shift picks the VCO that hits
    /// it best; t16-t20 fill the presets.
    const QUARTUS_SHIFTS: &[([Out; 3], (u8, u32, u32, u8, u8), [Option<(u32, u8, u8)>; 3])] = &[
        // q1
        (
            [Some((2, 1, -3000)), None, None],
            (1, 20, 1, 4, 0),
            [Some((10, 1, 0)), None, None],
        ),
        // q2
        (
            [Some((2, 1, 0)), None, Some((2, 1, -1500))],
            (1, 20, 1, 2, 4),
            [Some((10, 2, 4)), None, Some((10, 1, 0))],
        ),
        // q3
        (
            [Some((2, 1, 0)), None, Some((2, 1, 8500))],
            (1, 20, 1, 1, 0),
            [Some((10, 1, 0)), None, Some((10, 9, 4))],
        ),
        // r1
        (
            [Some((2, 1, 100)), None, None],
            (1, 20, 1, 1, 0),
            [Some((10, 1, 1)), None, None],
        ),
        // r2
        (
            [Some((2, 1, 700)), None, None],
            (1, 18, 1, 1, 0),
            [Some((9, 1, 5)), None, None],
        ),
        // r3
        (
            [Some((2, 1, 1250)), None, None],
            (1, 16, 1, 1, 0),
            [Some((8, 2, 0)), None, None],
        ),
        // r4
        (
            [Some((2, 1, -700)), None, None],
            (1, 18, 1, 1, 5),
            [Some((9, 1, 0)), None, None],
        ),
        // r5
        (
            [Some((1, 1, 333)), None, None],
            (1, 15, 1, 1, 0),
            [Some((15, 1, 2)), None, None],
        ),
        // r6
        (
            [Some((2, 1, 100)), Some((2, 1, 700)), None],
            (1, 18, 1, 1, 0),
            [Some((9, 1, 1)), Some((9, 1, 5)), None],
        ),
        // r7
        (
            [Some((2, 1, -100)), None, None],
            (1, 20, 1, 1, 1),
            [Some((10, 1, 0)), None, None],
        ),
        // r8
        (
            [Some((2, 1, -12000)), None, None],
            (1, 20, 1, 13, 0),
            [Some((10, 1, 0)), None, None],
        ),
        // r9
        (
            [Some((2, 1, 15000)), None, None],
            (1, 16, 1, 1, 0),
            [Some((8, 13, 0)), None, None],
        ),
        // r11
        (
            [Some((2, 1, -1500)), Some((1, 1, -1500)), None],
            (1, 20, 1, 2, 4),
            [Some((10, 1, 0)), Some((20, 1, 0)), None],
        ),
        // r12
        (
            [Some((1, 5, -2000)), None, None],
            (1, 6, 2, 1, 5),
            [Some((30, 1, 0)), None, None],
        ),
        // r13
        (
            [Some((2, 1, -1250)), Some((2, 1, 625)), None],
            (1, 16, 1, 2, 0),
            [Some((8, 1, 0)), Some((8, 2, 4)), None],
        ),
        // s1
        (
            [Some((1, 2, 2250)), None, None],
            (1, 10, 1, 1, 0),
            [Some((20, 2, 1)), None, None],
        ),
        // s4
        (
            [Some((1, 4, 2000)), None, None],
            (2, 15, 2, 1, 0),
            [Some((30, 1, 6)), None, None],
        ),
        // s7
        (
            [Some((2, 1, -20000)), None, None],
            (1, 14, 1, 15, 0),
            [Some((7, 1, 0)), None, None],
        ),
        // s8
        (
            [Some((2, 1, 20000)), None, None],
            (1, 14, 1, 1, 0),
            [Some((7, 15, 0)), None, None],
        ),
        // s9
        (
            [Some((2, 1, -700)), Some((2, 1, 700)), None],
            (1, 18, 1, 1, 5),
            [Some((9, 1, 0)), Some((9, 2, 2)), None],
        ),
        // s10
        (
            [Some((2, 1, 50)), Some((2, 1, 100)), None],
            (1, 20, 1, 1, 0),
            [Some((10, 1, 0)), Some((10, 1, 1)), None],
        ),
        // k1
        (
            [Some((1, 5, 417)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 1)), None, None],
        ),
        // k2
        (
            [Some((1, 5, 833)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 2)), None, None],
        ),
        // k3
        (
            [Some((1, 5, 1250)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 3)), None, None],
        ),
        // k4
        (
            [Some((1, 5, 1667)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 4)), None, None],
        ),
        // k5
        (
            [Some((1, 5, 2083)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 5)), None, None],
        ),
        // k6
        (
            [Some((1, 5, 2500)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 6)), None, None],
        ),
        // k7
        (
            [Some((1, 5, 2917)), None, None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 7)), None, None],
        ),
        // k8
        (
            [Some((1, 5, 833)), Some((1, 5, 2500)), None],
            (1, 6, 2, 1, 0),
            [Some((30, 1, 2)), Some((30, 1, 6)), None],
        ),
        // t16
        (
            [Some((2, 1, 18750)), None, None],
            (1, 16, 1, 1, 0),
            [Some((8, 16, 0)), None, None],
        ),
        // t17
        (
            [Some((2, 1, -18750)), None, None],
            (1, 16, 1, 16, 0),
            [Some((8, 1, 0)), None, None],
        ),
        // t18
        (
            [Some((2, 1, 18875)), None, None],
            (1, 16, 1, 1, 0),
            [Some((8, 16, 1)), None, None],
        ),
        // t19
        (
            [Some((2, 1, -18875)), None, None],
            (1, 16, 1, 16, 1),
            [Some((8, 1, 0)), None, None],
        ),
        // t20
        (
            [Some((2, 1, -15000)), Some((2, 1, 15000)), None],
            (1, 10, 1, 8, 4),
            [Some((5, 1, 0)), Some((5, 16, 0)), None],
        ),
    ];

    fn fit(s: &Solved) -> Fit {
        let c = &s.config;
        (
            (
                c.n,
                c.m.count(),
                if c.vco_div2 { 2 } else { 1 },
                c.m_initial,
                c.m_phase,
            ),
            c.outputs
                .map(|o| o.map(|o| (o.divider.map_or(1, Divider::count), o.initial, o.phase))),
        )
    }

    #[test]
    fn phase_shifts_as_quartus_fits_them() {
        for (outs, loop_, counters) in QUARTUS_SHIFTS {
            let r = Request {
                inclk_period_ps: 20_000,
                outputs: outs.map(|o| {
                    o.map(|(multiply, divide, phase_ps)| OutputRequest {
                        multiply,
                        divide,
                        phase_ps,
                        duty_percent: 50,
                    })
                }),
                feedback: Feedback::Normal,
            };
            let s = r.solve().unwrap_or_else(|e| panic!("{outs:?}: {e:#}"));
            assert_eq!(fit(&s), (*loop_, *counters), "{outs:?}");
        }
    }

    /// -1.5 ns on c2 next to c0 at 0 (sdram-selftest's DRAM_CLK the
    /// Quartus way): the feedback counter 1.5 VCO cycles late, c0 with it.
    #[test]
    fn negative_shift_delays_the_feedback_counter() {
        let mut r = req(&[(2, 1), (2, 1), (2, 1)]);
        r.outputs[1] = None;
        r.outputs[2].as_mut().unwrap().phase_ps = -1500;
        let s = r.solve().unwrap();
        assert_eq!((s.config.m_initial, s.config.m_phase), (2, 4));
        let c0 = s.config.outputs[0].unwrap();
        assert_eq!((c0.initial, c0.phase), (2, 4));
        let c2 = s.config.outputs[2].unwrap();
        assert_eq!((c2.initial, c2.phase), (1, 0));
        assert_eq!(s.phase_ps[0], Some(0.0));
        assert_eq!(s.phase_ps[2], Some(-1500.0));
    }

    /// A shift no tap hits is rounded to the nearest eighth of a VCO
    /// period: +700 ps at 100 MHz is 694 ps from a 900 MHz VCO (Quartus:
    /// "phase shift of 25 degrees (694 ps)").
    #[test]
    fn inexact_shift_is_rounded() {
        let mut r = req(&[(2, 1)]);
        r.outputs[0].as_mut().unwrap().phase_ps = -700;
        let s = r.solve().unwrap();
        assert_eq!(s.phase_ps[0].map(f64::round), Some(-694.0));
    }

    /// More delay than a preset holds at every VCO: an error. Quartus
    /// clamps it (t40: -40 ns at 100 MHz made -30 ns, warning 15559).
    #[test]
    fn shift_beyond_the_presets_is_refused() {
        let mut r = req(&[(2, 1)]);
        r.outputs[0].as_mut().unwrap().phase_ps = -40_000;
        let e = r.solve().unwrap_err().to_string();
        assert!(e.contains("16 VCO cycles"), "{e}");
    }

    /// The netlist JSON's 32-bit two's complement and decimal text.
    #[test]
    fn negative_phase_parameter() {
        let mut p = HashMap::new();
        p.insert("CLK0_MULTIPLY_BY".to_string(), "2".to_string());
        p.insert(
            "CLK0_PHASE_SHIFT".to_string(),
            format!("{:032b}", -1500i32 as u32),
        );
        let r = Request::from_params(&p, [true, false, false]).unwrap();
        assert_eq!(r.outputs[0].unwrap().phase_ps, -1500);
        p.insert("CLK0_PHASE_SHIFT".to_string(), "\"-3000\"".to_string());
        let r = Request::from_params(&p, [true, false, false]).unwrap();
        assert_eq!(r.outputs[0].unwrap().phase_ps, -3000);
    }

    #[test]
    fn impossible_ratios_are_refused() {
        assert!(req(&[(1, 997)]).solve().is_err());
    }
}
