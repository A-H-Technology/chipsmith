# `chipsmith.lib`. A function of `self` alone — `self.packages.<system>.default`
# and this set — so it can also be imported outside the flake, which is how
# the e2e test evaluates `buildProject` inside its VM.
{ self }:
{
  # `chipsmith.lib.buildProject { inherit pkgs; src = ./design; }`
  buildProject = import ./build-project.nix { inherit self; };

  # The system feature a builder advertises for a Toolchain, and that a
  # Project needing it requires.
  systemFeature = { backend, version }: "${backend}-${version}";

  # Where the NixOS module mounts Toolchains inside the build sandbox, and
  # what `buildProject` sets CHIPSMITH_INSTALL_ROOT to. Fixed, so a derivation
  # is the same whichever builder it lands on.
  sandboxInstallRoot = "/chipsmith";
}
