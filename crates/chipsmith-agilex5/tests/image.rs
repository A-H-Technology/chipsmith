//! Assembly against files known to be right: the open-flow designs whose
//! cores loaded on the DE25-Nano, and Quartus' own core for the same shell.
//! Needs what the repository can't hold, so it is ignored by default
//! (`cargo test -p chipsmith-agilex5 -- --ignored`):
//!
//! - `CHIPSMITH_QUARTUS_PRO`: a Quartus Prime Pro 26.1.1 install, for the
//!   SDM firmware and the codebooks (docs/legal.md).
//! - `CHIPSMITH_AGILEX5_GRAPH`: the routing graph agilex5-re dumps from that
//!   install (`specimens/rg/full`).
//! - `CHIPSMITH_AGILEX5_SPECIMENS`: agilex5-re's `specimens/`, the
//!   designs, the board-tested files and Quartus' builds.
use std::path::{Path, PathBuf};

use chipsmith_agilex5::{db::Db, rbf, rebuild, route, sof::Sof};

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

fn str(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Paths and keys every test needs.
struct Setup {
    db_root: PathBuf,
    hps_first: PathBuf,
    install: rbf::install::Install,
    rbf_key: Vec<u8>,
    phase1: rbf::Phase1,
}

impl Setup {
    fn new() -> Setup {
        let db_root = var("CHIPSMITH_AGILEX5_DB");
        let hps_first = var("CHIPSMITH_AGILEX5_SPECIMENS").join("hps-first");
        Setup {
            install: rbf::install::Install::open(&var("CHIPSMITH_QUARTUS_PRO")).unwrap(),
            rbf_key: std::fs::read(db_root.join("keys/rbf_keyed_hash.key")).unwrap(),
            phase1: rbf::Phase1::parse(str(&hps_first.join("out/hpsf_hps_auto.sof"))).unwrap(),
            db_root,
            hps_first,
        }
    }

    fn core(&self, db: &mut Db, sof: &Sof) -> Vec<u8> {
        rbf::write(db, &self.install, sof, &self.rbf_key, Some(&self.phase1)).unwrap()
    }

    fn expect(&self, file: &str) -> Vec<u8> {
        std::fs::read(self.hps_first.join(file)).unwrap()
    }
}

#[test]
#[ignore = "needs a Quartus Prime Pro install and agilex5-re's specimens"]
fn hps_first_cores_are_the_board_tested_ones() {
    let s = Setup::new();
    let mut db = Db::open(str(&s.db_root.join("db/sm4revb"))).unwrap();
    for (sof, expected) in [
        ("open/hpsf_open.sof", "a5/open.core.rbf"),
        ("rb/lut.sof", "a5/lut.core.rbf"),
        ("rb/rebuilt.sof", "a5/rebuilt.core.rbf"),
        ("out/hpsf.sof", "out/hpsf.core.rbf"),
    ] {
        let sof = Sof::read(str(&s.hps_first.join(sof))).unwrap();
        assert!(
            s.core(&mut db, &sof) == s.expect(expected),
            "{expected} differs"
        );
    }
}

/// A design file, assembled: the `.sof` the board-tested core was made from,
/// and that core.
#[test]
#[ignore = "needs a Quartus Prime Pro install, its routing graph and agilex5-re's specimens"]
fn designs_assemble_to_the_board_tested_files() {
    let s = Setup::new();
    let (mut db, graph) = route::open(
        str(&s.db_root.join("db/sm4revb")),
        str(&var("CHIPSMITH_AGILEX5_GRAPH")),
    )
    .unwrap();
    let sof_key = std::fs::read(s.db_root.join("keys/sof_hmac.key")).unwrap();
    let out = std::env::temp_dir().join(format!("chipsmith-agilex5-{}.sof", std::process::id()));
    for (design, sof, core) in [
        ("open/merged.txt", "open/hpsf_open.sof", "a5/open.core.rbf"),
        ("rb/design_lut.txt", "rb/lut.sof", "a5/lut.core.rbf"),
        ("rb/design.txt", "rb/rebuilt.sof", "a5/rebuilt.core.rbf"),
    ] {
        let text = std::fs::read_to_string(s.hps_first.join(design)).unwrap();
        let image = rebuild::build(&mut db, &graph, &text).unwrap();
        let mut built =
            Sof::from_raw(chipsmith_agilex5::sofgen::build(&s.install, &image).unwrap()).unwrap();
        built.write(str(&out), &sof_key).unwrap();
        assert!(
            std::fs::read(&out).unwrap() == s.expect(sof),
            "{design}: .sof differs from {sof}"
        );
        let built = Sof::read(str(&out)).unwrap();
        assert!(
            s.core(&mut db, &built) == s.expect(core),
            "{design}: core differs from {core}"
        );
    }
    std::fs::remove_file(&out).unwrap();
}
