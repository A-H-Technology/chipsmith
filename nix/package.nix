{
  lib,
  rustPlatform,
  runCommand,
  makeWrapper,
  pkg-config,
  libusb1,
  ghdl,
  # cyclone2-re's db package: the device and chip databases (ADR-0003).
  cyclone2-db,
  # nix/xc7.nix: prjxray-db (the 7-series tools are the devshell's oracles)
  xc7,
  # agilex5-re's packages: the Agilex 5 database, keys, routing graph and the
  # few Quartus Prime Pro files an image needs, the SDM firmware among them
  # (ADR-0003, docs/legal.md).
  agilex5-db,
  agilex5-graph,
  agilex5-quartus,
  # agilex5-re's phase1: each board's phase 1 shell lines and HPS IO hash,
  # for a manifest's `phase1 = "<board>"`.
  agilex5-phase1,
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
        ../crates
      ];
    };
    cargoLock.lockFile = ../Cargo.lock;
    # Unit tests only. The integration tests (crates/*/tests) check against
    # yosys as an oracle (ADR-0004): development tooling the package doesn't
    # ship, run in the devshell.
    cargoTestFlags = [
      "--lib"
      "--bins"
    ];

    nativeBuildInputs = [ pkg-config ];
    # the USB-Blaster driver (crates/chipsmith-jtag/src/usb_blaster.rs)
    buildInputs = [ libusb1 ];

    meta.mainProgram = "chipsmith";
  };

  # The router's graph, flattened from cyclone2-re's chip database once here
  # instead of on every build (parsing the text takes ~12 s, this ~2 s).
  routingGraph = runCommand "cyclone2-routing-graph" { } ''
    ${lib.getExe unwrapped} routing-graph ${cyclone2-db}/EP2C35F672C6/chipdb.txt.xz $out
  '';

  # The DE2's serial flash loader
  # (crates/chipsmith-cyclone2/src/persist/loader), built by chipsmith itself
  # once here, so `flash --persist` doesn't synthesise it every time. No
  # tools: the DE2 build runs none for Verilog.
  de2Loader =
    runCommand "chipsmith-de2-flash-loader"
      {
        CHIPSMITH_CYCLONE2_GRAPH = routingGraph;
        CHIPSMITH_CYCLONE2_DB = "${cyclone2-db}";
      }
      ''
        cp -r ${../crates/chipsmith-cyclone2/src/persist/loader} loader
        chmod -R u+w loader
        ${lib.getExe unwrapped} -C loader build
        install -D loader/build/loader.rbf $out/loader.rbf
      '';

  # The same for the Zybo's xc7z010, imported from prjxray-db (~15 s).
  xc7RoutingGraph = runCommand "xc7z010-routing-graph" { } ''
    ${lib.getExe unwrapped} xc7-routing-graph ${xc7.prjxray-db}/zynq7 $out
  '';
in
runCommand "chipsmith-${unwrapped.version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    passthru = {
      inherit
        unwrapped
        routingGraph
        xc7RoutingGraph
        de2Loader
        ;
    };
    meta.mainProgram = "chipsmith";
  }
  # Every stage chipsmith doesn't do itself yet, pinned here rather than
  # looked up on the host: GHDL translates VHDL for both devices (ADR-0001).
  # Neither build runs anything else.
  ''
    makeWrapper ${lib.getExe unwrapped} $out/bin/chipsmith \
      --prefix PATH : ${
        lib.makeBinPath [
          ghdl
        ]
      } \
      --set CHIPSMITH_CYCLONE2_GRAPH ${routingGraph} \
      --set CHIPSMITH_CYCLONE2_DB ${cyclone2-db} \
      --set CHIPSMITH_DE2_LOADER ${de2Loader}/loader.rbf \
      --set CHIPSMITH_XC7_GRAPH ${xc7RoutingGraph} \
      --set CHIPSMITH_PRJXRAY_DB ${xc7.prjxray-db} \
      --set CHIPSMITH_AGILEX5_DB ${agilex5-db} \
      --set CHIPSMITH_AGILEX5_GRAPH ${agilex5-graph} \
      --set CHIPSMITH_AGILEX5_QUARTUS ${agilex5-quartus} \
      --set CHIPSMITH_AGILEX5_PHASE1 ${agilex5-phase1}
  ''
