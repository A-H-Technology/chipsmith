//! Raw deflate exactly as quartus_pfg writes it: zlib level 9, 8 KiB window
//! (`windowBits` −13), `memLevel` 8, `Z_FIXED` (README (g), IO section).

use std::os::raw::c_int;

use anyhow::{ensure, Result};
use libz_sys as z;

// zlib's allocator hooks; a size header lets zfree hand the layout back.
extern "C" fn zalloc(_: z::voidpf, items: z::uInt, size: z::uInt) -> z::voidpf {
    let n = items as usize * size as usize + 16;
    unsafe {
        let p = std::alloc::alloc(std::alloc::Layout::from_size_align_unchecked(n, 16));
        if p.is_null() {
            return std::ptr::null_mut();
        }
        (p as *mut usize).write(n);
        p.add(16) as z::voidpf
    }
}

extern "C" fn zfree(_: z::voidpf, p: z::voidpf) {
    if p.is_null() {
        return;
    }
    unsafe {
        let base = (p as *mut u8).sub(16);
        let n = (base as *mut usize).read();
        std::alloc::dealloc(base, std::alloc::Layout::from_size_align_unchecked(n, 16));
    }
}

pub fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![0u8; data.len() + data.len() / 8 + 1024];
    unsafe {
        let mut s = z::z_stream {
            next_in: std::ptr::null_mut(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            zalloc,
            zfree,
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        };
        let rc = z::deflateInit2_(
            &mut s,
            9,
            z::Z_DEFLATED,
            -13,
            8,
            z::Z_FIXED,
            z::zlibVersion(),
            std::mem::size_of::<z::z_stream>() as c_int,
        );
        ensure!(rc == z::Z_OK, "deflateInit2 failed: {rc}");
        s.next_in = data.as_ptr() as *mut _;
        s.avail_in = data.len() as _;
        s.next_out = out.as_mut_ptr();
        s.avail_out = out.len() as _;
        let rc = z::deflate(&mut s, z::Z_FINISH);
        let n = s.total_out as usize;
        z::deflateEnd(&mut s);
        ensure!(rc == z::Z_STREAM_END, "deflate did not finish: {rc}");
        out.truncate(n);
    }
    Ok(out)
}

/// Inflate a raw deflate stream; returns (data, bytes consumed).
pub fn inflate(data: &[u8]) -> Result<(Vec<u8>, usize)> {
    let mut d = flate2::Decompress::new(false);
    let mut out = Vec::with_capacity(data.len() * 4);
    loop {
        out.reserve(1 << 20);
        let before = d.total_in();
        let st = d.decompress_vec(
            &data[d.total_in() as usize..],
            &mut out,
            flate2::FlushDecompress::None,
        )?;
        if st == flate2::Status::StreamEnd {
            return Ok((out, d.total_in() as usize));
        }
        ensure!(
            d.total_in() != before || out.len() < out.capacity(),
            "truncated deflate stream"
        );
    }
}
