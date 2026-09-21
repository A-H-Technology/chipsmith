use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::{Manifest, PinMapping};

pub fn generate_qpf(manifest: &Manifest) -> String {
    format!(
        "QUARTUS_VERSION = \"{ver}\"\n\
         DATE = \"00:00:00 January 01, 2024\"\n\
         PROJECT_REVISION = \"{name}\"\n",
        ver = manifest.toolchain.version(),
        name = manifest.project.name,
    )
}

pub fn generate_qsf(
    manifest: &Manifest,
    sources: &[PathBuf],
    project_dir: &Path,
) -> Result<String, ChipsmithError> {
    let mut qsf = String::new();

    writeln!(
        qsf,
        "set_global_assignment -name FAMILY \"{}\"",
        manifest.target.family
    )
    .unwrap();
    writeln!(
        qsf,
        "set_global_assignment -name DEVICE {}",
        manifest.target.device
    )
    .unwrap();
    writeln!(
        qsf,
        "set_global_assignment -name TOP_LEVEL_ENTITY {}",
        manifest.project.top
    )
    .unwrap();
    writeln!(
        qsf,
        "set_global_assignment -name VHDL_INPUT_VERSION {}",
        manifest.hdl.standard
    )
    .unwrap();
    writeln!(
        qsf,
        "set_global_assignment -name PROJECT_OUTPUT_DIRECTORY output_files"
    )
    .unwrap();
    writeln!(
        qsf,
        "set_global_assignment -name NUM_PARALLEL_PROCESSORS ALL"
    )
    .unwrap();
    writeln!(qsf).unwrap();

    for source in sources {
        let relative = source.strip_prefix(project_dir).unwrap_or(source);
        writeln!(
            qsf,
            "set_global_assignment -name VHDL_FILE ../{}",
            relative.display()
        )
        .unwrap();
    }
    if !manifest.clocks.is_empty() {
        writeln!(
            qsf,
            "set_global_assignment -name SDC_FILE {}.sdc",
            manifest.project.name
        )
        .unwrap();
    }
    writeln!(qsf).unwrap();

    for (signal, mapping) in &manifest.pins {
        let io_standard = manifest.io_standard_for(signal);
        match mapping {
            PinMapping::Single(pin) => {
                writeln!(qsf, "set_location_assignment {pin} -to {signal}").unwrap();
                if let Some(standard) = io_standard {
                    writeln!(
                        qsf,
                        "set_instance_assignment -name IO_STANDARD \"{standard}\" -to {signal}"
                    )
                    .unwrap();
                }
            }
            PinMapping::Bus(pins) => {
                for (i, pin) in pins.iter().enumerate() {
                    writeln!(qsf, "set_location_assignment {pin} -to {signal}[{i}]").unwrap();
                    if let Some(standard) = io_standard {
                        writeln!(
                            qsf,
                            "set_instance_assignment -name IO_STANDARD \"{standard}\" -to {signal}[{i}]"
                        )
                        .unwrap();
                    }
                }
            }
        }
    }

    Ok(qsf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_toolchain::manifest::{Hdl, Project, Target, ToolchainSpec};
    use std::collections::BTreeMap;

    fn test_manifest() -> Manifest {
        Manifest {
            project: Project {
                name: "blinky".to_string(),
                top: "blinky".to_string(),
            },
            toolchain: ToolchainSpec::new("quartus-prime", "23.1"),
            target: Target {
                family: "Cyclone V".to_string(),
                device: "5CSEBA6U23I7".to_string(),
                io_standard: None,
            },
            hdl: Hdl {
                standard: "VHDL_2008".to_string(),
                sources: vec!["src/*.vhd".to_string()],
            },
            pins: BTreeMap::new(),
            clocks: BTreeMap::new(),
            io_standards: BTreeMap::new(),
        }
    }

    #[test]
    fn qpf_contains_version_and_name() {
        let manifest = test_manifest();
        let qpf = generate_qpf(&manifest);
        assert!(qpf.contains("QUARTUS_VERSION = \"23.1\""));
        assert!(qpf.contains("PROJECT_REVISION = \"blinky\""));
    }

    #[test]
    fn qsf_contains_device_and_family() {
        let manifest = test_manifest();
        let sources = vec![PathBuf::from("/proj/src/blinky.vhd")];
        let qsf = generate_qsf(&manifest, &sources, Path::new("/proj")).unwrap();

        assert!(qsf.contains("FAMILY \"Cyclone V\""));
        assert!(qsf.contains("DEVICE 5CSEBA6U23I7"));
        assert!(qsf.contains("TOP_LEVEL_ENTITY blinky"));
        assert!(qsf.contains("VHDL_INPUT_VERSION VHDL_2008"));
        assert!(qsf.contains("VHDL_FILE ../src/blinky.vhd"));
    }

    #[test]
    fn qsf_references_sdc_only_when_clocks_are_declared() {
        let mut manifest = test_manifest();
        let without = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(!without.contains("SDC_FILE"));

        manifest
            .clocks
            .insert("clk".to_string(), "50 MHz".to_string());
        let with = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(with.contains("set_global_assignment -name SDC_FILE blinky.sdc"));
    }

    #[test]
    fn qsf_single_pin_assignment() {
        let mut manifest = test_manifest();
        manifest
            .pins
            .insert("clk".to_string(), PinMapping::Single("PIN_Y2".to_string()));

        let qsf = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(qsf.contains("set_location_assignment PIN_Y2 -to clk"));
    }

    #[test]
    fn qsf_omits_io_standard_when_unset() {
        let mut manifest = test_manifest();
        manifest
            .pins
            .insert("clk".to_string(), PinMapping::Single("PIN_Y2".to_string()));

        let qsf = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(!qsf.contains("IO_STANDARD"));
    }

    #[test]
    fn qsf_applies_target_io_standard_to_every_pin_including_bus_members() {
        let mut manifest = test_manifest();
        manifest.target.io_standard = Some("3.3-V LVTTL".to_string());
        manifest
            .pins
            .insert("clk".to_string(), PinMapping::Single("PIN_Y2".to_string()));
        manifest.pins.insert(
            "led".to_string(),
            PinMapping::Bus(vec!["PIN_V16".to_string(), "PIN_W16".to_string()]),
        );

        let qsf = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(qsf.contains("set_instance_assignment -name IO_STANDARD \"3.3-V LVTTL\" -to clk"));
        assert!(
            qsf.contains("set_instance_assignment -name IO_STANDARD \"3.3-V LVTTL\" -to led[0]")
        );
        assert!(
            qsf.contains("set_instance_assignment -name IO_STANDARD \"3.3-V LVTTL\" -to led[1]")
        );
    }

    #[test]
    fn qsf_per_signal_io_standard_overrides_the_target_default() {
        let mut manifest = test_manifest();
        manifest.target.io_standard = Some("3.3-V LVTTL".to_string());
        manifest
            .pins
            .insert("clk".to_string(), PinMapping::Single("PIN_Y2".to_string()));
        manifest
            .io_standards
            .insert("clk".to_string(), "1.5 V".to_string());

        let qsf = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(qsf.contains("set_instance_assignment -name IO_STANDARD \"1.5 V\" -to clk"));
        assert!(!qsf.contains("3.3-V LVTTL"));
    }

    #[test]
    fn qsf_bus_pin_assignment() {
        let mut manifest = test_manifest();
        manifest.pins.insert(
            "led".to_string(),
            PinMapping::Bus(vec![
                "PIN_V16".to_string(),
                "PIN_W16".to_string(),
                "PIN_V17".to_string(),
            ]),
        );

        let qsf = generate_qsf(&manifest, &[], Path::new("/proj")).unwrap();
        assert!(qsf.contains("set_location_assignment PIN_V16 -to led[0]"));
        assert!(qsf.contains("set_location_assignment PIN_W16 -to led[1]"));
        assert!(qsf.contains("set_location_assignment PIN_V17 -to led[2]"));
    }
}
