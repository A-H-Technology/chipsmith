//! Synthesis against agilex5-re's a5synth, whose netlists it must reproduce:
//! `CHIPSMITH_AGILEX5_PNR_REF` holds, per design with a `<name>.sources`
//! list, a5synth's netlist (`<name>.json`). The netlists name their source
//! files in `src` attributes, which depend on where the sources were read
//! from, so those lines aren't compared. Ignored by default (the designs
//! and netlists are agilex5-re's specimens):
//! `cargo test -p chipsmith-agilex5 -- --ignored`.
use std::path::{Path, PathBuf};

use chipsmith_agilex5::synth;

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

fn without_src(json: &str) -> Vec<&str> {
    json.lines()
        .filter(|l| !l.trim_start().starts_with("\"src\": "))
        .collect()
}

#[test]
#[ignore = "needs agilex5-re's designs and a5synth's netlists"]
fn designs_synthesize_as_a5synth_did() {
    let refs = var("CHIPSMITH_AGILEX5_PNR_REF");
    let out = std::env::temp_dir().join(format!(
        "chipsmith-agilex5-synth-{}.json",
        std::process::id()
    ));
    let mut names: Vec<String> = std::fs::read_dir(&refs)
        .unwrap()
        .filter_map(|e| {
            e.unwrap()
                .file_name()
                .to_str()?
                .strip_suffix(".sources")
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no designs in {}", refs.display());
    for name in &names {
        let list = std::fs::read_to_string(refs.join(format!("{name}.sources"))).unwrap();
        let sources: Vec<&Path> = list.lines().map(Path::new).collect();
        synth::run(name, &sources, &out, &synth::Options::default()).unwrap();
        let got = std::fs::read_to_string(&out).unwrap();
        let want = std::fs::read_to_string(refs.join(format!("{name}.json"))).unwrap();
        assert!(
            without_src(&got) == without_src(&want),
            "{name}: netlist differs"
        );
    }
    std::fs::remove_file(&out).unwrap();
}
