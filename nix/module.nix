# Declares which Toolchains this machine can build with.
#
# One list drives both halves of the contract: the system feature a
# `buildProject` derivation requires, and the sandbox mount that puts the
# Toolchain where that derivation looks for it. A machine cannot advertise a
# Toolchain without also mounting it.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.chipsmith;
  inherit (lib) mkOption types;

  toolchain = types.submodule {
    options = {
      backend = mkOption {
        type = types.str;
        example = "quartus-ii-13";
        description = "The Backend, as a Manifest's `[toolchain]` names it.";
      };
      version = mkOption {
        type = types.str;
        example = "13.0sp1";
        description = "The Version, as a Manifest's `[toolchain]` names it.";
      };
      path = mkOption {
        # A string, not `types.path`: a path literal would be copied into the
        # store, and a Quartus install is tens of gigabytes of unfree software.
        type = types.addCheck types.str (lib.hasPrefix "/") // {
          description = "absolute path outside the nix store";
        };
        example = "/home/alice/altera/13.0sp1";
        description = ''
          Where the Version is installed on this machine, e.g. what
          `chipsmith which --backend <backend> <version>` prints. It has to be
          patched by the same chipsmith as `package` (`chipsmith install` with
          it re-patches an existing install), and readable by the nix build
          users.
        '';
      };
    };
  };

  feature = t: self.lib.systemFeature { inherit (t) backend version; };
  mount = t: "${self.lib.sandboxInstallRoot}/${t.backend}/${t.version}=${t.path}";
  keys = map (t: "${t.backend}/${t.version}") cfg.toolchains;
in
{
  options.programs.chipsmith = {
    enable = lib.mkEnableOption "chipsmith, and building chipsmith Projects as nix derivations";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "chipsmith.packages.\${system}.default";
      description = ''
        The chipsmith on PATH. Installs it patches name the same store paths
        as the chipsmith that `buildProject` runs in the sandbox, which is what
        lets a patched Toolchain run there at all.
      '';
    };

    toolchains = mkOption {
      type = types.listOf toolchain;
      default = [ ];
      example = lib.literalExpression ''
        [ { backend = "quartus-ii-13"; version = "13.0sp1"; path = "/home/alice/altera/13.0sp1"; } ]
      '';
      description = ''
        Toolchains this machine builds with. Each becomes the system feature
        `<backend>-<version>` and is mounted into the build sandbox at
        `${self.lib.sandboxInstallRoot}/<backend>/<version>`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.length keys == lib.length (lib.unique keys);
        message = "programs.chipsmith.toolchains lists the same backend and version twice";
      }
    ];

    environment.systemPackages = [ cfg.package ];

    # NixOS sets its defaults (kvm, big-parallel, ...) in config rather than
    # only as the option default, so these merge with them instead of
    # replacing them. The e2e test checks that stays true.
    nix.settings.system-features = map feature cfg.toolchains;

    # No `?` suffix: a declared Toolchain that is missing should fail the build
    # loudly, not hand Quartus an empty directory.
    nix.settings.extra-sandbox-paths = map mount cfg.toolchains;
  };
}
