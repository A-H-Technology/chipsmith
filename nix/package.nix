{
  lib,
  rustPlatform,
  runCommand,
  makeWrapper,
  pkg-config,
  libusb1,
  ghdl,
  yosys,
  cram,
  cyclone2-synth,
}:

let
  unwrapped = rustPlatform.buildRustPackage {
    pname = "chipsmith-unwrapped";
    version = "0.1.0";

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../src
      ];
    };
    cargoLock.lockFile = ../Cargo.lock;

    nativeBuildInputs = [ pkg-config ];
    # the USB-Blaster driver (src/jtag/usb_blaster.rs)
    buildInputs = [ libusb1 ];

    meta.mainProgram = "chipsmith";
  };

  # The router's graph, flattened from cyclone2-re's chip database once here
  # instead of on every build (parsing the text takes ~12 s, this ~2 s).
  routingGraph = runCommand "cyclone2-routing-graph" { } ''
    ${lib.getExe unwrapped} routing-graph ${cram}/db/EP2C35F672C6/chipdb.txt.xz $out
  '';
in
runCommand "chipsmith-${unwrapped.version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    passthru = { inherit unwrapped routingGraph; };
    meta.mainProgram = "chipsmith";
  }
  # Every stage chipsmith doesn't do itself yet, pinned here rather than
  # looked up on the host.
  ''
    makeWrapper ${lib.getExe unwrapped} $out/bin/chipsmith \
      --prefix PATH : ${
        lib.makeBinPath [
          ghdl
          yosys
          cram
        ]
      } \
      --set CHIPSMITH_CYCLONE2_SYNTH ${cyclone2-synth} \
      --set CHIPSMITH_CYCLONE2_GRAPH ${routingGraph} \
      --set CHIPSMITH_CYCLONE2_DB ${cram}/db
  ''
