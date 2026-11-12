//! Place and route against agilex5-re's a5pnr, whose results it must
//! reproduce: `CHIPSMITH_AGILEX5_PNR_REF` holds, per design, the netlist
//! (`<name>.json`), its pins (`<name>.pins`) and a5pnr's design file and
//! flop map (`<name>.design.txt`, `.design.txt.ffmap`). One of them,
//! hpsf_open, is the design whose core loaded on the DE25-Nano. The
//! references are agilex5-re's specimens, so it is ignored by default:
//! `cargo test -p chipsmith-agilex5 -- --ignored`.
use std::path::PathBuf;

use chipsmith_agilex5::pnr;

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

/// A design file without its first line, which names the netlist's path.
fn body(text: &str) -> &str {
    text.split_once('\n').map_or("", |(_, rest)| rest)
}

#[test]
#[ignore = "needs the routing graph dumped from Quartus and a5pnr's results"]
fn designs_place_and_route_as_a5pnr_did() {
    let db = var("CHIPSMITH_AGILEX5_DB").join("db/sm4revb");
    let graph = var("CHIPSMITH_AGILEX5_GRAPH");
    let refs = var("CHIPSMITH_AGILEX5_PNR_REF");
    let out = std::env::temp_dir().join(format!("chipsmith-agilex5-pnr-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&refs)
        .unwrap()
        .filter_map(|e| {
            e.unwrap()
                .file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no designs in {}", refs.display());
    let s = |p: &PathBuf| p.to_str().unwrap().to_owned();
    for name in &names {
        let design = out.join(format!("{name}.design.txt"));
        pnr::run(
            &s(&db),
            &s(&graph),
            &s(&db.join("timing/delays.tsv")),
            &s(&refs.join(format!("{name}.json"))),
            name,
            &s(&refs.join(format!("{name}.pins"))),
            &s(&design),
            &pnr::Options::default(),
        )
        .unwrap();
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert!(
            body(&read(design.clone())) == body(&read(refs.join(format!("{name}.design.txt")))),
            "{name}: design file differs"
        );
        assert!(
            read(out.join(format!("{name}.design.txt.ffmap")))
                == read(refs.join(format!("{name}.design.txt.ffmap"))),
            "{name}: flop map differs"
        );
    }
    std::fs::remove_dir_all(&out).unwrap();
}
