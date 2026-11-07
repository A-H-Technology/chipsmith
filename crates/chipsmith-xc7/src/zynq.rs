//! The Zynq's `.bin` (what its devcfg driver, Linux' fpga-manager, loads)
//! and the boot image its BootROM reads from flash.

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

/// The BootROM header's size; the loader follows it (UG585 section 6.3.2,
/// U-Boot's tools/zynqimage.c).
pub const BOOT_HEADER_LEN: usize = 0x8c0;
/// On-chip memory the BootROM copies the loader into, from address 0.
const OCM_LEN: usize = 192 * 1024;

/// A boot image: the header, then `loader`, which the BootROM copies into
/// OCM at 0 and starts at 0. Unencrypted, no register init list.
pub fn boot_image(loader: &[u8]) -> Result<Vec<u8>> {
    ensure!(!loader.is_empty(), "an empty first-stage loader");
    ensure!(
        loader.len().is_multiple_of(4),
        "the loader is {} bytes, not whole words",
        loader.len()
    );
    ensure!(
        loader.len() <= OCM_LEN,
        "the loader is {} bytes; OCM holds {OCM_LEN}",
        loader.len()
    );
    let mut image = boot_header(loader.len() as u32).to_vec();
    image.extend_from_slice(loader);
    Ok(image)
}

fn boot_header(image_len: u32) -> [u8; BOOT_HEADER_LEN] {
    let mut words = [0u32; BOOT_HEADER_LEN / 4];
    // Interrupt vectors: branch-to-self, as bootgen and mkimage write them.
    words[..8].fill(0xeaff_fffe);
    words[0x20 / 4] = 0xaa99_5566; // width detection
    words[0x24 / 4] = u32::from_le_bytes(*b"XNLX"); // reads "XLNX" as a big-endian word
    words[0x28 / 4] = 0; // not encrypted
    words[0x30 / 4] = BOOT_HEADER_LEN as u32; // source offset
    words[0x34 / 4] = image_len;
    words[0x38 / 4] = 0; // load address: OCM
    words[0x3c / 4] = 0; // execution start
    words[0x40 / 4] = image_len; // total length, the same when unencrypted
    words[0x48 / 4] = !words[0x20 / 4..0x48 / 4]
        .iter()
        .fold(0u32, |a, &w| a.wrapping_add(w));
    // Register init list: empty, every pair 0xffffffff.
    words[0xa0 / 4..0x8a0 / 4].fill(0xffff_ffff);
    let mut bytes = [0u8; BOOT_HEADER_LEN];
    for (b, w) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        b.copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mkimage -T zynqimage` (u-boot-tools 2026.10) on 32 bytes 0x10..0x2f.
    /// mkimage writes the whole file's length (header included) as the
    /// image length; with that length the header is the same byte for byte.
    #[test]
    fn boot_header_is_mkimages() {
        let fixture = include_bytes!("../tests/data/zynqimage-32.bin");
        let payload: Vec<u8> = (0x10..0x30).collect();
        let mut ours = boot_header(fixture.len() as u32).to_vec();
        ours.extend_from_slice(&payload);
        assert_eq!(ours, fixture);
    }

    #[test]
    fn boot_image_carries_the_loaders_own_length() {
        let image = boot_image(&[0; 64]).unwrap();
        assert_eq!(image.len(), BOOT_HEADER_LEN + 64);
        let word = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap());
        assert_eq!((word(0x30), word(0x34), word(0x40)), (0x8c0, 64, 64));
        let sum = (0x20..0x48)
            .step_by(4)
            .fold(0u32, |a, at| a.wrapping_add(word(at)));
        assert_eq!(word(0x48), !sum);
        assert!(boot_image(&[]).is_err());
        assert!(boot_image(&[0; 3]).is_err());
        assert!(boot_image(&vec![0; OCM_LEN + 4]).is_err());
    }

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
