//! A PLL as a design asks for it (`CY2_PLL`, `synth/cells.v`): an input
//! clock period and, per output, a multiply/divide ratio, phase shift and
//! duty cycle, altpll's way of saying it. `solve` finds the counters that
//! make exactly those frequencies (ADR-0008).
//!
//! The limits are Quartus' own for the EP2C35 (its `cycloneii_pll` atoms
//! carry them, cyclone2-re's PLL fuzz designs): the VCO between 500 and
//! 1000 MHz, the phase detector between 10 and 402.5 MHz; n is 1-4, m and
//! each output counter 1-32 (a high and a low count of 1-16 each).

use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};

use crate::bitstream::pll::{Config, Divider, Feedback, Loop, Output};

/// VCO and phase-detector limits, in hertz.
const VCO_MIN_HZ: f64 = 500e6;
const VCO_MAX_HZ: f64 = 1000e6;
const PFD_MIN_HZ: f64 = 10e6;
const PFD_MAX_HZ: f64 = 402.5e6;
/// The VCO frequency Quartus' choices sit closest to (cyclone2-re README,
/// "PLLs": 800 MHz for 50, 100 and 400 MHz out of 50 MHz).
const VCO_PREFERRED_HZ: f64 = 800e6;

/// One output as asked for: `f_in * multiply / divide`, shifted by
/// `phase_ps`, high for `duty_percent` of the period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRequest {
    pub multiply: u32,
    pub divide: u32,
    pub phase_ps: u32,
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
            *o = Some(OutputRequest {
                multiply: num(&format!("CLK{k}_MULTIPLY_BY"), 1)?,
                divide: num(&format!("CLK{k}_DIVIDE_BY"), 1)?,
                phase_ps: num(&format!("CLK{k}_PHASE_SHIFT"), 0)?,
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
    /// their limits. Of those, the VCO post-scale of 1, a VCO nearest
    /// 800 MHz, then the smallest n, as Quartus picks.
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
        // (score, (post-scale, n, m, counters))
        type Candidate = ((u32, u64, u32), (u32, u32, u32, [u32; 3]));
        let mut best: Option<Candidate> = None;
        for k in 1..=2u32 {
            for n in 1..=4u32 {
                let pfd = fin / f64::from(n);
                if !(PFD_MIN_HZ..=PFD_MAX_HZ).contains(&pfd) {
                    continue;
                }
                // m >= 4: the lock window below that was never seen.
                for m in 4..=32u32 {
                    let vco = pfd * f64::from(m * k);
                    if !(VCO_MIN_HZ..=VCO_MAX_HZ).contains(&vco) {
                        continue;
                    }
                    // Counter k of output o: c = m * divide / (n * multiply),
                    // an integer.
                    let mut c = [0u32; 3];
                    let ok = used.iter().all(|&(i, o)| {
                        let num = u64::from(m) * u64::from(o.divide);
                        let den = u64::from(n) * u64::from(o.multiply);
                        if num % den != 0 {
                            return false;
                        }
                        c[i] = (num / den) as u32;
                        (1..=32).contains(&c[i])
                    });
                    if !ok {
                        continue;
                    }
                    let score = (k, (vco - VCO_PREFERRED_HZ).abs() as u64, n);
                    if best.as_ref().is_none_or(|(b, _)| score < *b) {
                        best = Some((score, (k, n, m, c)));
                    }
                }
            }
        }
        let Some((_, (k, n, m, c))) = best else {
            bail!(
                "CY2_PLL: no counters make {} from {:.3} MHz with the VCO at 500-1000 MHz",
                used.iter()
                    .map(|(i, o)| format!(
                        "CLK{i} = {:.3} MHz",
                        fin * f64::from(o.multiply) / f64::from(o.divide) / 1e6
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
                fin / 1e6
            )
        };
        // The counters run on the VCO divided by the post-scale.
        let fvco = fin * f64::from(m) / f64::from(n);
        let mut outputs = [None; 3];
        let mut hz = [None; 3];
        for &(i, o) in &used {
            let count = c[i];
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
            // Phase in eighths of a VCO period: whole periods into the
            // counter's preset, the rest onto a VCO tap (fuzz p5, p16).
            let taps = (f64::from(o.phase_ps) * 1e-12 * fvco * 8.0).round() as u32;
            let initial = taps / 8 + 1;
            ensure!(
                initial <= 16,
                "CY2_PLL CLK{i}: phase shift {} ps is too large",
                o.phase_ps
            );
            outputs[i] = Some(Output {
                divider,
                initial: initial as u8,
                phase: (taps % 8) as u8,
            });
            hz[i] = Some(fvco / f64::from(count));
        }
        Ok(Solved {
            config: Config {
                n: n as u8,
                m: Divider::new(m.div_ceil(2) as u8, (m / 2) as u8, m % 2 == 1)?,
                m_initial: 1,
                m_phase: 0,
                vco_div2: k == 2,
                outputs,
                feedback: self.feedback,
                r#loop: Loop::for_m(m * k),
            },
            hz,
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

    #[test]
    fn impossible_ratios_are_refused() {
        assert!(req(&[(1, 997)]).solve().is_err());
    }
}
