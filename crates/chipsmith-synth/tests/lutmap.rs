//! The LUT mapper on the IR maps as it does on yosys' JSON (what the build
//! runs today): the same gates, the same LUTs, the same depth. Needs yosys
//! and `CHIPSMITH_CYCLONE2_SYNTH`: run in the devshell.

use std::path::{Path, PathBuf};
use std::process::Command;

use chipsmith_netlist::{read_json, CellKind};

/// cyclone2-re's `cyclone2.ys` up to its LUT mapping, on picorv32.
fn gates(dir: &Path) -> PathBuf {
    let synth =
        PathBuf::from(std::env::var("CHIPSMITH_CYCLONE2_SYNTH").expect("run in `nix develop`"));
    let script = std::fs::read_to_string(synth.join("synth/cyclone2.ys")).unwrap();
    let lines: Vec<&str> = script.lines().collect();
    let cut = lines.iter().position(|l| l.trim() == "abc -lut 4").unwrap();
    let corpus =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../chipsmith-netlist/tests/corpus/picorv32");
    let out = dir.join("gates.json");
    let ys = dir.join("gates.ys");
    std::fs::write(
        &ys,
        format!(
            "read_verilog {} {}\n{}\nwrite_json {}\n",
            corpus.join("picorv32.v").display(),
            corpus.join("top.v").display(),
            lines[..cut].join("\n"),
            out.display()
        ),
    )
    .unwrap();
    let status = Command::new("yosys")
        .args(["-q", "-s"])
        .arg(&ys)
        .current_dir(&synth)
        .status()
        .expect("yosys on PATH: run in `nix develop`");
    assert!(status.success());
    out
}

#[test]
fn ir_and_json_map_alike() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lutmap");
    std::fs::create_dir_all(&dir).unwrap();
    let gates = gates(&dir);
    let json_stats = chipsmith_synth::lutmap::map_file(&gates, &dir.join("luts.json")).unwrap();
    let mut design = read_json(&std::fs::read(&gates).unwrap()).unwrap();
    let top = design.modules.iter_mut().find(|m| m.name == "top").unwrap();
    let ir_stats = chipsmith_synth::lutmap::map_module(top).unwrap();
    let luts = top
        .cells
        .iter()
        .filter(|c| matches!(c.kind, CellKind::Lut(_)))
        .count();
    eprintln!("JSON {json_stats:?}\nIR   {ir_stats:?}");
    assert_eq!(ir_stats.luts, luts);
    assert_eq!(ir_stats.gates, json_stats.gates);
    assert_eq!(ir_stats.and_nodes, json_stats.and_nodes);
    assert_eq!(ir_stats.luts, json_stats.luts);
    assert_eq!(ir_stats.depth, json_stats.depth);
}

/// LUTs of `top` by input count.
fn by_inputs(top: &chipsmith_netlist::Module) -> [usize; 7] {
    let mut n = [0; 7];
    for c in &top.cells {
        if let CellKind::Lut(l) = &c.kind {
            n[l.a.len()] += 1;
        }
    }
    n
}

/// At K = 6 a cost that charges a 6-input LUT two smaller ones trades
/// 6-input LUTs for smaller ones and lowers that cost; keeping the best cut
/// never deepens the mapping.
#[test]
fn cost_and_kept_cut_at_six() {
    use chipsmith_synth::lutmap::{map_module_k, map_module_with, LutCost, LutSize, MapOptions};
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lutmap-cost");
    std::fs::create_dir_all(&dir).unwrap();
    let gates = std::fs::read(gates(&dir)).unwrap();
    let map = |opts: Option<MapOptions>| {
        let mut design = read_json(&gates).unwrap();
        let top = design.modules.iter_mut().find(|m| m.name == "top").unwrap();
        let stats = match opts {
            Some(o) => map_module_with(top, LutSize::SIX, o).unwrap(),
            None => map_module_k(top, LutSize::SIX).unwrap(),
        };
        (stats, by_inputs(top))
    };
    let (plain, n0) = map(None);
    let (default, n1) = map(Some(MapOptions::DEFAULT));
    assert_eq!((plain, n0), (default, n1));
    let alm = MapOptions {
        cost: LutCost::by_inputs([3, 3, 3, 3, 3, 4, 6]).unwrap(),
        keep_best_cut: true,
    };
    let (costed, n2) = map(Some(alm));
    let area = |n: [usize; 7]| 3 * n[..5].iter().sum::<usize>() + 4 * n[5] + 6 * n[6];
    eprintln!(
        "uniform {n0:?} depth {}\ncosted  {n2:?} depth {}",
        plain.depth, costed.depth
    );
    assert!(n2[6] < n0[6]);
    assert!(area(n2) < area(n0));
    let (kept, _) = map(Some(MapOptions {
        keep_best_cut: true,
        ..MapOptions::DEFAULT
    }));
    assert!(kept.depth <= plain.depth);
    assert!(LutCost::by_inputs([1, 1, 1, 0, 1, 1, 1]).is_none());
}
