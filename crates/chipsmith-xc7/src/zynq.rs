//! The Zynq's `.bin`: what its devcfg driver (Linux' fpga-manager) loads.

use anyhow::{ensure, Result};

/// A `.bit` payload as the Zynq's devcfg driver takes it (Xilinx calls it a
/// byte-swapped `.bin`): every 32-bit word little-endian, so the sync word
/// reads `66 55 99 aa`. The PCAP DMA moves whole words.
pub fn zynq_bin(payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len().is_multiple_of(4),
        ".bit payload is {} bytes, not whole 32-bit words",
        payload.len()
    );
    Ok(payload
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[a, b, c, d]| [d, c, b, a])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zynq_bin_swaps_each_word() {
        let sync = [0xff, 0xff, 0xff, 0xff, 0xaa, 0x99, 0x55, 0x66];
        assert_eq!(
            zynq_bin(&sync).unwrap(),
            [0xff, 0xff, 0xff, 0xff, 0x66, 0x55, 0x99, 0xaa]
        );
        assert!(zynq_bin(&[0xaa, 0x99, 0x55]).is_err());
    }
}
