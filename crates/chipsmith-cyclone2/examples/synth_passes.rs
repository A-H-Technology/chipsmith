//! Debugging aid: runs named chipsmith-synth passes in order on a netlist:
//! `synth_passes <in.json> <out.json> <pass>...`.

use chipsmith_netlist::{read_json, write_json};
use chipsmith_synth::{alumacc, clean, memory, opt, undef, wreduce};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[3] == "lutmap-file" {
        let s = chipsmith_synth::lutmap::map_file(args[1].as_ref(), args[2].as_ref()).unwrap();
        eprintln!("{s:?}");
        return;
    }
    let mut d = read_json(&std::fs::read(&args[1]).unwrap()).unwrap();
    d.modules.retain(|m| !m.is_blackbox());
    let m = &mut d.modules[0];
    let rm = chipsmith_cyclone2::synth::cells::REMOVABLE;
    for p in &args[3..] {
        match p.as_str() {
            "refine" => undef::refine(m).unwrap(),
            "collect" => memory::collect(m).unwrap(),
            "lower" => opt::lower_bitwise(m).unwrap(),
            "gates" => {
                opt::opt_gates(m);
            }
            "word" => {
                opt::opt_word(m);
            }
            "merge" => {
                opt::opt_merge(m);
            }
            "muxtree" => {
                opt::opt_muxtree(m);
            }
            "dff" => {
                opt::opt_dff(m);
            }
            "clean" => clean::clean(m, rm),
            "opt" => opt::opt(m, rm),
            "seqconst" => {
                let c = opt::seqconst::find(m).unwrap();
                eprintln!("seqconst: {}", opt::seqconst::apply(m, &c));
            }
            "wreduce" => {
                wreduce::wreduce(m);
            }
            "alumacc" => alumacc::alumacc(m),
            other => panic!("unknown pass {other}"),
        }
    }
    write_json(&d, std::fs::File::create(&args[2]).unwrap()).unwrap();
}
