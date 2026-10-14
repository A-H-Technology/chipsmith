# A chipsmith Project as a derivation: `$out` holds the Bitstream and the
# timing report. Only builds on a machine whose `programs.chipsmith.toolchains`
# provides the Manifest's Toolchain.
#
# Not bit-reproducible: Quartus stamps dates into its output, so
# `nix build --rebuild` will report a difference. That is expected.
{ self }:
{
  pkgs,
  src,
  # Fail the derivation, rather than warn, if the design misses timing.
  requireTiming ? false,
}:

let
  # The Manifest stays the single source of truth for which Toolchain.
  manifest = builtins.fromTOML (builtins.readFile (src + "/chipsmith.toml"));
  backends = builtins.attrNames manifest.toolchain;
  backend =
    if builtins.length backends == 1 then
      builtins.head backends
    else
      throw "${toString src}/chipsmith.toml: [toolchain] must have exactly one entry";
  version = manifest.toolchain.${backend};
  systemFeature = self.lib.systemFeature { inherit backend version; };
  chipsmith = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
in
pkgs.stdenvNoCC.mkDerivation {
  name = "${manifest.project.name}-bitstream";
  inherit src;

  nativeBuildInputs = [ chipsmith ];
  requiredSystemFeatures = [ systemFeature ];

  dontConfigure = true;
  dontInstall = true;
  dontFixup = true;

  buildPhase = ''
    runHook preBuild

    # Quartus writes ~/.altera.quartus, and the sandbox's $HOME does not exist.
    export HOME=$TMPDIR
    export CHIPSMITH_INSTALL_ROOT=${self.lib.sandboxInstallRoot}
    chipsmith build --project-dir . --out-dir $out ${pkgs.lib.optionalString requireTiming "--require-timing"}

    runHook postBuild
  '';

  passthru = { inherit backend version systemFeature; };
}
