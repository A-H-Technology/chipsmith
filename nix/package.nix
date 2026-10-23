{
  lib,
  rustPlatform,
  makeWrapper,
  pkg-config,
  libusb1,
  ghdl,
  yosys,
  nextpnr-cyclone2,
  cram,
  cyclone2-synth,
}:

rustPlatform.buildRustPackage {
  pname = "chipsmith";
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

  nativeBuildInputs = [
    makeWrapper
    pkg-config
  ];
  # the USB-Blaster driver (src/jtag/usb_blaster.rs)
  buildInputs = [ libusb1 ];

  # Every stage chipsmith doesn't do itself yet, pinned here rather than
  # looked up on the host.
  postInstall = ''
    wrapProgram $out/bin/chipsmith \
      --prefix PATH : ${
        lib.makeBinPath [
          ghdl
          yosys
          nextpnr-cyclone2
          cram
        ]
      } \
      --set CHIPSMITH_CYCLONE2_SYNTH ${cyclone2-synth}
  '';

  meta.mainProgram = "chipsmith";
}
