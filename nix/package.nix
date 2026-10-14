{
  lib,
  pkgs,
  rustPlatform,
  makeWrapper,
  unzip,
}:

rustPlatform.buildRustPackage {
  pname = "chipsmith";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      # a test builds plans for the shipped examples
      ../example/chipsmith.toml
      ../example/src
      ../example-de2/chipsmith.toml
      ../example-de2/src
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "chipsmith-cli"
  ];
  cargoTestFlags = [ "--workspace" ];

  # The Nix Compat paths, fixed at build time. Being strings in the binary
  # makes them runtime references, so they are in chipsmith's closure: present
  # in any build sandbox chipsmith runs in, and kept alive as long as it is.
  # An install patched by this chipsmith names exactly these.
  env.CHIPSMITH_NIX_COMPAT = builtins.toJSON (
    import ../crates/chipsmith-quartus-common/compat.nix { inherit pkgs; }
  );

  nativeBuildInputs = [ makeWrapper ];

  # Device Support packages are zip files; chipsmith shells out to unpack them.
  postInstall = ''
    wrapProgram $out/bin/chipsmith --prefix PATH : ${lib.makeBinPath [ unzip ]}
  '';

  meta = {
    description = "Declarative FPGA toolchain management and builds";
    homepage = "https://github.com/cramt/chipsmith";
    license = lib.licenses.mit;
    mainProgram = "chipsmith";
    platforms = [ "x86_64-linux" ];
  };
}
