//! The routing graph's loaded arrays as one binary file, so a build reads
//! them instead of parsing agilex5-re's 285 MB `nodes.tsv` and 377 MB
//! `edges.bin` again (about 20 s per load). The nix package makes the cache
//! once from the graph package (`chipsmith agilex5-graph-cache`); without
//! it the loaders parse the text as before. A file is a magic line, then
//! arrays: a little-endian u64 element count and the elements.

use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

pub(crate) struct Writer(Vec<u8>);

impl Writer {
    pub fn new(magic: &str) -> Writer {
        Writer(format!("{magic}\n").into_bytes())
    }

    fn len(&mut self, n: usize) {
        self.0.extend_from_slice(&(n as u64).to_le_bytes());
    }

    pub fn u8s(&mut self, v: &[u8]) {
        self.len(v.len());
        self.0.extend_from_slice(v);
    }

    pub fn u16s(&mut self, v: &[u16]) {
        self.len(v.len());
        v.iter()
            .for_each(|x| self.0.extend_from_slice(&x.to_le_bytes()));
    }

    pub fn u32s(&mut self, v: &[u32]) {
        self.len(v.len());
        v.iter()
            .for_each(|x| self.0.extend_from_slice(&x.to_le_bytes()));
    }

    /// Strings as their lengths, then their bytes back to back.
    pub fn strs(&mut self, v: &[String]) {
        self.u32s(&v.iter().map(|s| s.len() as u32).collect::<Vec<_>>());
        self.len(v.iter().map(String::len).sum());
        v.iter()
            .for_each(|s| self.0.extend_from_slice(s.as_bytes()));
    }

    pub fn save(self, path: &Path) -> Result<()> {
        std::fs::write(path, self.0).with_context(|| format!("writing {}", path.display()))
    }
}

pub(crate) struct Reader {
    buf: Vec<u8>,
    at: usize,
}

impl Reader {
    /// The file at `path`, if it is a cache with this `magic`.
    pub fn open(path: &Path, magic: &str) -> Result<Option<Reader>> {
        let Ok(buf) = std::fs::read(path) else {
            return Ok(None);
        };
        let head = format!("{magic}\n");
        if !buf.starts_with(head.as_bytes()) {
            bail!("{}: not a {magic} cache", path.display());
        }
        Ok(Some(Reader {
            at: head.len(),
            buf,
        }))
    }

    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(self.at + n <= self.buf.len(), "truncated graph cache");
        let s = &self.buf[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    fn len(&mut self) -> Result<usize> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?) as usize)
    }

    pub fn u8s(&mut self) -> Result<Vec<u8>> {
        let n = self.len()?;
        Ok(self.take(n)?.to_vec())
    }

    pub fn u16s(&mut self) -> Result<Vec<u16>> {
        let n = self.len()?;
        Ok(self
            .take(2 * n)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect())
    }

    pub fn u32s(&mut self) -> Result<Vec<u32>> {
        let n = self.len()?;
        Ok(self
            .take(4 * n)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect())
    }

    pub fn strs(&mut self) -> Result<Vec<String>> {
        let lens = self.u32s()?;
        let total = self.len()?;
        let bytes = self.take(total)?;
        let mut at = 0;
        lens.iter()
            .map(|&l| {
                let s = std::str::from_utf8(&bytes[at..at + l as usize])?.to_owned();
                at += l as usize;
                Ok(s)
            })
            .collect()
    }

    pub fn done(&self) -> Result<()> {
        ensure!(self.at == self.buf.len(), "graph cache has trailing bytes");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_read_back_as_written() {
        let path = std::env::temp_dir().join(format!("chipsmith-cache-{}", std::process::id()));
        let mut w = Writer::new("test 1");
        w.u8s(&[1, 2, 3]);
        w.u16s(&[0xbeef, 7]);
        w.u32s(&[0xdead_beef]);
        w.strs(&["LAB_RE:X1Y2Z0I3".into(), String::new(), "é".into()]);
        w.save(&path).unwrap();
        let mut r = Reader::open(&path, "test 1").unwrap().unwrap();
        assert_eq!(r.u8s().unwrap(), [1, 2, 3]);
        assert_eq!(r.u16s().unwrap(), [0xbeef, 7]);
        assert_eq!(r.u32s().unwrap(), [0xdead_beef]);
        assert_eq!(r.strs().unwrap(), ["LAB_RE:X1Y2Z0I3", "", "é"]);
        r.done().unwrap();
        // Another magic is an error, not a silent misread; no file is None.
        assert!(Reader::open(&path, "test 2").is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(Reader::open(&path, "test 1").unwrap().is_none());
    }
}
