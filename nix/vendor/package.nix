{
  lib,
  pkgs,
  rustPlatform,
  makeWrapper,
  unzip,
}:

rustPlatform.buildRustPackage {
  pname = "chipsmith-vendor";
  version = (lib.importTOML ../../Cargo.toml).workspace.package.version;

  src = lib.fileset.toSource {
    root = ../../.;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../crates
    ];
  };
  cargoLock.lockFile = ../../Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "chipsmith-vendor"
  ];
  # The vendor crates only: the open ones are the chipsmith package's.
  cargoTestFlags = [
    "-p"
    "chipsmith-vendor"
    "-p"
    "chipsmith-core"
    "-p"
    "chipsmith-quartus-common"
    "-p"
    "chipsmith-quartus-ii-13"
    "-p"
    "chipsmith-quartus-prime"
    "-p"
    "chipsmith-quartus-pro"
    "-p"
    "chipsmith-sim"
    "-p"
    "chipsmith-toolchain"
  ];

  # The Nix Compat paths, fixed at build time. Being strings in the binary
  # makes them runtime references, so they are in chipsmith's closure: present
  # in any build sandbox chipsmith runs in, and kept alive as long as it is.
  # An install patched by this chipsmith names exactly these.
  env.CHIPSMITH_NIX_COMPAT = builtins.toJSON (
    import ../../crates/vendor-quartus-common/compat.nix { inherit pkgs; }
  );

  nativeBuildInputs = [ makeWrapper ];

  # Device Support packages are zip files; chipsmith shells out to unpack them.
  postInstall = ''
    wrapProgram $out/bin/chipsmith-vendor --prefix PATH : ${lib.makeBinPath [ unzip ]}
  '';

  meta = {
    description = "Declarative FPGA toolchain management and builds";
    homepage = "https://github.com/cramt/chipsmith";
    license = lib.licenses.mit;
    mainProgram = "chipsmith-vendor";
    platforms = [ "x86_64-linux" ];
  };
}
