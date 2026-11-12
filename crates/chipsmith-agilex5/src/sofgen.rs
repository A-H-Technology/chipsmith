//! A `.sof` from nothing but an image: every record Quartus's assembler
//! writes, built from the install and the rules found in README (n), so the
//! container has no Quartus output in it.
//!
//! Records, in order: 0x01 version, 0x02 part, 0x2f die and snapshot, 0x03
//! title, 0x13 (16 constant bytes), 0x36 `sof_meta.pb` with its header and
//! record sizes filled in, 0x3a the image, 0x24 the SLD file archive, 0x3a
//! HANDOFF (HPS handoff, no HPS), 0x3a OPTIONS, 0x3a PROGRAMMING_SEQUENCE
//! (the install's `programming_sequence.pb.enc`), 0x41 HMAC, 0x08 CRC.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::rbf::install::{pb_parse, Install, Pb};

const PART: &str = "a5eb013bb23be4scs";
/// The project title Quartus writes when there is none.
const TITLE: &str = "Untitled";
/// Record 0x13: the same 16 bytes in every specimen [unk].
const R13: [u8; 16] = [
    0, 0, 0, 0, 0x20, 0, 0x20, 0, 0, 0, 1, 0, 0xff, 0xff, 0xff, 0xff,
];
/// JTAG IDCODE of the A5EB013B, as the SLD archive's `jtag_id.txt` holds it.
pub const IDCODE: u32 = 0xc362_c0dd;

/// OPTIONS (record 0x3a index 3): the device options of a project with the
/// default settings and DEVICE_INITIALIZATION_CLOCK OSC_CLK_1_125MHZ; the
/// same in every specimen except field 1.20, the IO hash as upper-case hex,
/// which goes between the two halves.
const OPTIONS_HEAD: &str = concat!(
    "0ab40508c0b2cd3b1080c2d72f180522b6020a6908011000180a200a28003000380c40004a0208004a0208004a020800",
    "4a0208004a0208004a0208004a0208004a0208005206080010001800580260f4ffffffffffffffff0168009001009801",
    "00a00108a801ff01b001dd15b801d41dc00101d00100da01040800100012040a024e41120a0a0841535f444154413112",
    "080a0641535f434c4b120a0a0841535f4441544132120a0a0841535f4441544130120a0a0841535f6e43534f30120a0a",
    "0841535f4441544133120a0a0841535f6e43534f32120a0a0841535f6e43534f33120a0a0841535f6e43534f3112040a",
    "024e4112040a024e4112040a024e4112040a024e4112040a024e41120a0a0841535f524553455412040a024e41300038",
    "01400048904e500058006000680070008001018a0100900100980100a00100a80100b00100b8010028ffffffff0f3800",
    "424039306436343165643639613365393964383738363633666534383138613432643331336334353163626335656562",
    "3231306333323336636134646664656635374a2608001000180120002800300138014001480050006803700078008001",
    "0088010090010098010050005800600068007000880101a20140",
);
const OPTIONS_TAIL: &str = concat!(
    "aa0140303030303131313132323232333333333434343435353535363636363737373738383838393939394141414142",
    "42424243434343444444444545454546464646b2011608001000180020002800300038004000480050005800ba012b08",
    "00100028003023386e40d8ffffffffffffffff0148005800680f70007800800100880100900100980100c80100da010e",
    "0a0012001a0022002a0032003a00f00100f80100800200b80278c002001a1808001214534f4620464541545552452054",
    "5241434b494e47",
);

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn pb_int(out: &mut Vec<u8>, f: u64, v: u64) {
    varint(out, f << 3);
    varint(out, v);
}
fn pb_bytes(out: &mut Vec<u8>, f: u64, v: &[u8]) {
    varint(out, f << 3 | 2);
    varint(out, v.len() as u64);
    out.extend_from_slice(v);
}

/// IO hash: SHA-256 over both IO96 objects (README (n)).
pub fn io_hash(img: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    for obj in ["io96_0", "io96_1"] {
        for (o, _, start, n) in crate::db::PARTS {
            if *o == obj {
                h.update(&img[*start..start + n]);
            }
        }
    }
    h.finalize().into()
}

/// a5db's design hash: Quartus takes it from the netlist database
/// (`flow_get_design_hash`), so a writer with no netlist picks its own; this
/// one is SHA-256 over both sector objects (header, CRAM, RAM).
pub fn design_hash(img: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    for (o, _, start, n) in crate::db::PARTS {
        if o.starts_with("sector") {
            h.update(&img[*start..start + n]);
        }
    }
    h.finalize().into()
}

/// HANDOFF (record 0x3a index 2): the HPS handoff of a design without HPS.
/// 48 HPS pins, each with a pin-mux record (9: unused), an I/O record and a
/// delay record, then the binary handoff blob: a BOOT header, then sections
/// PMUX, IOCT (per pin: register offset, value 0x34), FPGA, DLAY (per pin:
/// offset, 0), CLKS, PERI and SDRM, all big endian, then 16 zero bytes.
fn handoff() -> Vec<u8> {
    const PINS: usize = 48;
    // IOCT register offsets: pins 0..27 at 0x00.., 28..47 at 0xd0..
    let ioct = |k: usize| if k < 28 { 4 * k } else { 0xd0 + 4 * (k - 28) } as u32;
    let mut out = Vec::new();
    for _ in 0..PINS {
        out.extend_from_slice(&[0x1a, 0x02, 0x08, 0x09]);
    }
    let io = hex("0838102c18002000280030023802400048015001580160006800");
    for _ in 0..PINS {
        pb_bytes(&mut out, 4, &io);
    }
    for _ in 0..PINS {
        pb_bytes(&mut out, 5, &[0x08, 0x00, 0x10, 0x00]);
    }
    let mut blob = b"BOOT".to_vec();
    blob.extend_from_slice(&[0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut section = |magic: &[u8; 4], len: usize, pairs: &[(u32, u32)]| {
        let start = blob.len();
        blob.extend_from_slice(magic);
        blob.extend_from_slice(&(len as u32).to_be_bytes());
        blob.extend_from_slice(&[0; 8]);
        for (a, v) in pairs {
            blob.extend_from_slice(&a.to_be_bytes());
            blob.extend_from_slice(&v.to_be_bytes());
        }
        blob.resize(start + len, 0);
    };
    section(b"PMUX", 400, &[]);
    section(
        b"IOCT",
        400,
        &(0..PINS).map(|k| (ioct(k), 0x34)).collect::<Vec<_>>(),
    );
    section(b"FPGA", 192, &[]);
    section(
        b"DLAY",
        400,
        &(0..PINS).map(|k| (4 * k as u32, 0)).collect::<Vec<_>>(),
    );
    section(b"CLKS", 160, &[]);
    section(b"PERI", 20, &[]);
    section(b"SDRM", 20, &[]);
    blob.extend_from_slice(&[0; 16]);
    let mut inner = Vec::new();
    pb_bytes(&mut inner, 1, &blob);
    pb_bytes(&mut out, 7, &inner);
    out
}

/// The SLD archive (record 0x24): `(u32 stamp, u16 0, u32 name length, u16
/// 0, u32 data length, name, data)` entries; the stamp is the `.sld` file's
/// mtime in Quartus's and 0 for the others (0 everywhere here). Quartus puts the
/// 10-byte SLD design hash, the project's `.sld` file and the IDCODE in it.
fn sld_archive(design_hash: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut entry = |name: &str, data: &[u8]| {
        let mut n = name.as_bytes().to_vec();
        n.push(0);
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&(n.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&n);
        out.extend_from_slice(data);
    };
    let mut h = vec![0u8; 2];
    h.extend_from_slice(&design_hash[..10]);
    h.extend_from_slice(&[0; 4]);
    entry("design_hash.bin", &h);
    entry("output_files/a5db.sld", b"<sld_project_info/>\n");
    entry("jtag_id.txt", format!("0x{IDCODE:08X}").as_bytes());
    out
}

/// One record descriptor in 0x36 field 2.11: (name, kind, size class, tag).
fn descriptor(out: &mut Vec<u8>, name: &str, kind: u64, class: u64, tag: Option<u64>) {
    let mut d = Vec::new();
    pb_bytes(&mut d, 1, name.as_bytes());
    pb_int(&mut d, 2, kind);
    pb_int(&mut d, 3, class);
    if let Some(t) = tag {
        pb_int(&mut d, 4, t);
    }
    pb_bytes(out, 11, &d);
}

/// Record 0x36: sof_meta.pb with field 2 (the file header) rebuilt and the
/// sizes of the other data records added to field 3 after the image's.
fn metadata(inst: &Install, sizes: &[usize], design_hash: &[u8; 32]) -> Result<Vec<u8>> {
    let meta = pb_parse(&inst.sof_meta).context("sof_meta.pb")?;
    let string = |f: u64| -> Result<&[u8]> {
        let Some((_, Pb::Bytes(hdr))) = meta.iter().find(|(g, _)| *g == 2) else {
            bail!("sof_meta.pb has no header")
        };
        for (g, v) in pb_parse(hdr).context("sof_meta.pb header")? {
            if let (true, Pb::Bytes(b)) = (g == f, v) {
                return Ok(b);
            }
        }
        bail!("sof_meta.pb header has no field {f}")
    };
    let mut hdr = Vec::new();
    pb_bytes(&mut hdr, 1, string(1)?);
    pb_int(&mut hdr, 2, 0);
    pb_bytes(&mut hdr, 3, inst.version.as_bytes());
    pb_bytes(&mut hdr, 4, PART.as_bytes());
    pb_bytes(&mut hdr, 5, string(5)?);
    pb_bytes(&mut hdr, 6, string(6)?);
    pb_bytes(&mut hdr, 7, TITLE.as_bytes());
    pb_int(&mut hdr, 8, 0xffff_ffff);
    pb_bytes(&mut hdr, 10, design_hash);
    descriptor(&mut hdr, "SIGNALTAP_DEBUG_FILES", 2, 20, Some(0x24));
    descriptor(&mut hdr, "HANDOFF", 1, 10, None);
    descriptor(&mut hdr, "OPTIONS", 1, 10, None);
    descriptor(&mut hdr, "PROGRAMMING_SEQUENCE", 7, 63, None);
    pb_int(&mut hdr, 12, 0xffff_ffff);
    let mut out = Vec::new();
    pb_bytes(&mut out, 2, &hdr);
    // The rest of sof_meta.pb verbatim, field 3 with the sizes after its
    // first entry.
    let mut p = 0;
    let raw = &inst.sof_meta;
    let skip = |p: &mut usize| -> Option<(u64, usize, usize)> {
        let (mut k, mut s) = (0u64, 0);
        let start = *p;
        loop {
            let c = *raw.get(*p)?;
            *p += 1;
            k |= ((c & 0x7f) as u64) << s;
            s += 7;
            if c < 0x80 {
                break;
            }
        }
        let (mut n, mut s) = (0usize, 0);
        loop {
            let c = *raw.get(*p)?;
            *p += 1;
            n |= ((c & 0x7f) as usize) << s;
            s += 7;
            if c < 0x80 {
                break;
            }
        }
        let body = *p;
        *p += n;
        Some((k >> 3, start, body))
    };
    while p < raw.len() {
        let (f, start, body) = skip(&mut p).context("sof_meta.pb framing")?;
        match f {
            2 => {}
            3 => {
                let fields = &raw[body..p];
                let first = pb_parse(fields).context("sof_meta.pb field 3")?;
                let size = match first.first() {
                    Some((1, Pb::Int(size))) => *size,
                    _ => bail!("sof_meta.pb field 3 does not start with the image size"),
                };
                let mut f3 = Vec::new();
                pb_int(&mut f3, 1, size);
                let head = f3.len();
                for &s in sizes {
                    pb_int(&mut f3, 1, s as u64);
                }
                f3.extend_from_slice(&fields[head..]);
                pb_bytes(&mut out, 3, &f3);
            }
            _ => out.extend_from_slice(&raw[start..p]),
        }
    }
    Ok(out)
}

fn record(out: &mut Vec<u8>, tag: u16, data: &[u8]) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

/// The uncompressed `.sof` for an image; `Sof::write` adds HMAC and CRC.
pub fn build(inst: &Install, img: &[u8]) -> Result<Vec<u8>> {
    let dh = design_hash(img);
    let ioh: String = io_hash(img).iter().map(|b| format!("{b:02X}")).collect();
    let indexed =
        |i: u32, pb: Vec<u8>| -> Vec<u8> { i.to_le_bytes().into_iter().chain(pb).collect() };
    let sld = sld_archive(&dh);
    let handoff = indexed(2, handoff());
    let mut options = hex(OPTIONS_HEAD);
    options.extend_from_slice(ioh.as_bytes());
    options.extend(hex(OPTIONS_TAIL));
    let options = indexed(3, options);
    let progseq = indexed(4, inst.programming_sequence.clone());
    let sizes = [
        sld.len(),
        handoff.len() - 4,
        options.len() - 4,
        progseq.len() - 4,
    ];
    let meta = metadata(inst, &sizes, &dh)?;
    let mut out = b"SOF\0".to_vec();
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&13u32.to_le_bytes());
    let cstr = |s: &str| -> Vec<u8> { s.bytes().chain([0]).collect() };
    record(&mut out, 0x01, &cstr(&inst.version));
    record(&mut out, 0x02, &cstr(PART));
    let die = |f: &str| -> Vec<u8> {
        let s = cstr(f);
        (s.len() as u16)
            .to_le_bytes()
            .into_iter()
            .chain(s)
            .collect()
    };
    let snap = {
        let Some((_, Pb::Bytes(h))) = pb_parse(&inst.sof_meta)
            .unwrap_or_default()
            .into_iter()
            .find(|(g, _)| *g == 2)
        else {
            bail!("no header")
        };
        let f = pb_parse(h).unwrap_or_default();
        let get = |n: u64| {
            f.iter().find_map(|(g, v)| match v {
                Pb::Bytes(b) if *g == n => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            })
        };
        (get(5).context("no die")?, get(6).context("no snapshot")?)
    };
    record(&mut out, 0x2f, &[die(&snap.0), die(&snap.1)].concat());
    record(&mut out, 0x03, &cstr(TITLE));
    record(&mut out, 0x13, &R13);
    record(&mut out, 0x36, &meta);
    record(&mut out, 0x3a, &indexed(0, img.to_vec()));
    record(&mut out, 0x24, &sld);
    record(&mut out, 0x3a, &handoff);
    record(&mut out, 0x3a, &options);
    record(&mut out, 0x3a, &progseq);
    record(&mut out, 0x41, &[0; 40]);
    record(&mut out, 0x08, &[0; 2]);
    Ok(out)
}
