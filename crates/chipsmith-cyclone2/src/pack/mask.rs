//! LUT mask arithmetic. A mask's bit `idx` is the output for inputs
//! A = idx bit 0 .. D = idx bit 3; in a carry LE input C is the carry in,
//! the half with D = 0 is the carry out and the half with D = 1 the sum
//! (yosys ties DATAD high, so COMBOUT reads it).

/// `mask` with input `i` replaced by input `j` XOR `inv`, or by the
/// constant `inv` when `j` is None.
pub fn substitute(mask: u16, i: usize, j: Option<usize>, inv: bool) -> u16 {
    let mut out = 0;
    for idx in 0..16 {
        let v = j.map_or(0, |j| (idx >> j) & 1) ^ inv as usize;
        let src = (idx & !(1 << i)) | (v << i);
        out |= ((mask >> src) & 1) << idx;
    }
    out
}

/// `mask` with input `i` tied to `value`.
pub fn fix(mask: u16, i: usize, value: bool) -> u16 {
    substitute(mask, i, None, value)
}

/// `mask` reading input `i` inverted.
pub fn invert_input(mask: u16, i: usize) -> u16 {
    substitute(mask, i, Some(i), true)
}

/// The one-input LUT masks: A, B, C or D passed through.
pub const BUFFERS: [u16; 4] = [0xAAAA, 0xCCCC, 0xF0F0, 0xFF00];

/// Quartus re-expresses every carry chain with the carry inverted after
/// each odd chain position (observed: counters from quartus_map, and
/// chains fed back to it as an atom netlist come out rewritten so), in the
/// last LE copies the sum into the unused carry half, and clears a sum half
/// nothing uses. Both forms compute the same thing; using Quartus' makes
/// the bitstream byte-comparable with what it assembles from the same
/// placement.
///
/// `position` is the LE's index in its chain; `last_unused_cout` that it
/// ends the chain with nothing on COUT, `sum_used` that COMBOUT has loads.
pub fn normalise_carry(mask: u16, position: usize, last_unused_cout: bool, sum_used: bool) -> u16 {
    let in_inv = position >= 2 && position.is_multiple_of(2);
    let out_inv = position % 2 == 1;
    let mut out = 0u16;
    for idx in 0..16 {
        let mut v = (mask >> if in_inv { idx ^ 4 } else { idx }) & 1;
        if idx < 8 && out_inv {
            v ^= 1;
        }
        out |= v << idx;
    }
    if last_unused_cout {
        out = (out & 0xFF00) | (out >> 8);
    }
    // An unused sum half is all zeros (comparators: only the carry out of
    // the top is used).
    if !sum_used {
        out &= 0x00FF;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(mask: u16, a: bool, b: bool, c: bool, d: bool) -> bool {
        let idx = a as u16 | (b as u16) << 1 | (c as u16) << 2 | (d as u16) << 3;
        (mask >> idx) & 1 == 1
    }

    #[test]
    fn a_constant_input_folds_into_the_mask() {
        let and_ab = 0x8888; // A & B
        assert_eq!(fix(and_ab, 0, true), BUFFERS[1]);
        assert_eq!(fix(and_ab, 0, false), 0);
        // The folded input no longer matters.
        let m = fix(0x6996, 2, true); // A ^ B ^ C ^ D, C = 1
        for idx in 0..16u16 {
            assert_eq!((m >> idx) & 1, (m >> (idx ^ 4)) & 1);
        }
    }

    #[test]
    fn an_inverter_folds_into_its_load() {
        assert_eq!(invert_input(BUFFERS[0], 0), !BUFFERS[0]);
        // (!A) & B
        assert_eq!(invert_input(0x8888, 0), 0x4444);
    }

    #[test]
    fn merging_an_input_onto_another() {
        // A ^ B with B replaced by A is 0; with B replaced by !A it is 1.
        assert_eq!(substitute(0x6666, 1, Some(0), false), 0);
        assert_eq!(substitute(0x6666, 1, Some(0), true), 0xFFFF);
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    /// Runs a chain: each LE's carry out is the next one's C. Returns every
    /// sum.
    fn run(masks: &[u16], a: &[bool], b: &[bool], cin: bool) -> Vec<bool> {
        let mut c = cin;
        let mut sums = Vec::new();
        for (k, &m) in masks.iter().enumerate() {
            sums.push(eval(m, a[k], b[k], c, true));
            c = eval(m, a[k], b[k], c, false);
        }
        sums
    }

    #[test]
    fn carry_polarity_normalisation_keeps_every_sum() {
        let mut rng = Lcg(1);
        for len in 1..9 {
            for _ in 0..20 {
                let masks: Vec<u16> = (0..len).map(|_| rng.next() as u16).collect();
                let norm: Vec<u16> = masks
                    .iter()
                    .enumerate()
                    .map(|(k, &m)| normalise_carry(m, k, k + 1 == len, true))
                    .collect();
                for v in 0..(1u32 << (2 * len + 1)) {
                    let a: Vec<bool> = (0..len).map(|k| v >> k & 1 == 1).collect();
                    let b: Vec<bool> = (0..len).map(|k| v >> (len + k) & 1 == 1).collect();
                    let cin = v >> (2 * len) & 1 == 1;
                    assert_eq!(run(&masks, &a, &b, cin), run(&norm, &a, &b, cin));
                }
            }
        }
    }

    #[test]
    fn carry_polarity_inverts_after_odd_positions() {
        let m = 0x6688;
        assert_eq!(normalise_carry(m, 0, false, true), m);
        // Odd: carry half inverted.
        assert_eq!(normalise_carry(m, 1, false, true), 0x6677);
        // Even after that: reads the carry in inverted (C flipped).
        assert_eq!(normalise_carry(0x96E8, 2, false, true), 0x698E);
        // Last LE: the carry half holds the sum; unused sum half: zeros.
        assert_eq!(normalise_carry(0x9600, 0, true, true), 0x9696);
        assert_eq!(normalise_carry(0x96E8, 0, false, false), 0x00E8);
    }
}
