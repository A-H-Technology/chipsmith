{ self, pkgs }:

let
  chipsmith = self.packages.${pkgs.stdenv.hostPlatform.system}.default;

  # One Toolchain, start to finish, inside a VM: `chipsmith install` patches it
  # as an ordinary user, the NixOS module declares it, and a `buildProject`
  # derivation for `project` is realised by the VM's own nix daemon — so the
  # Toolchain runs inside a real build sandbox, through the system feature and
  # the sandbox mount, and has to produce a Bitstream there.
  e2e =
    {
      backend,
      version,
      project,
      # The files `chipsmith install` would download, keyed by file name. They
      # are staged into the user's download cache, so the VM needs no network.
      downloads,
      # Synthesis of a large family wants more than the default VM.
      memorySize ? 8192,
      diskSize ? 30 * 1024,
    }:
    let
      installRoot = "/var/lib/quartus";
      system = pkgs.stdenv.hostPlatform.system;
      bitstream = self.lib.buildProject {
        inherit pkgs;
        src = project;
      };
      # The same `buildProject`, evaluated by the VM against the same nixpkgs
      # and chipsmith. Shipping the host's .drv instead would mean registering
      # its closure in the VM, and closureInfo includes the outputs of every
      # derivation in it, back through the stdenv bootstrap.
      inVm = pkgs.writeText "build-${backend}-${version}.nix" ''
        let
          self = {
            packages.${system}.default = builtins.storePath ${chipsmith};
            lib = import ${../.}/lib.nix { inherit self; };
          };
        in
        self.lib.buildProject {
          pkgs = import ${pkgs.path} { system = "${system}"; };
          src = builtins.storePath ${project};
        }
      '';
      stageDownloads = pkgs.lib.concatMapStringsSep "\n" (
        name:
        # Copied rather than linked: chipsmith marks an installer executable,
        # and a store path cannot be chmodded.
        "cp ${downloads.${name}} ~/.cache/chipsmith/${name}"
      ) (builtins.attrNames downloads);
    in
    pkgs.testers.runNixOSTest {
      name = "chipsmith-e2e-${backend}-${version}";

      nodes.builder = {
        imports = [ self.nixosModules.default ];

        programs.chipsmith = {
          enable = true;
          toolchains = [
            {
              inherit backend version;
              path = "${installRoot}/${backend}/${version}";
            }
          ];
        };

        users.users.alice.isNormalUser = true;
        systemd.tmpfiles.rules = [ "d ${installRoot} 0755 alice users -" ];

        virtualisation = {
          inherit memorySize diskSize;
          cores = 4;
          writableStore = true;
          # Everything the derivation needs except the Toolchain, so the VM
          # can evaluate and realise it offline.
          additionalPaths = [
            bitstream.inputDerivation
            inVm
            pkgs.path
          ]
          ++ builtins.attrValues downloads;
        };
        nix.settings.substituters = pkgs.lib.mkForce [ ];
      };

      testScript = ''
        builder.wait_for_unit("multi-user.target")

        with subtest("the module keeps NixOS's own system features"):
            features = builder.succeed("grep '^system-features' /etc/nix/nix.conf")
            assert "${bitstream.systemFeature}" in features, features
            assert "nixos-test" in features, features
            builder.succeed("grep '^extra-sandbox-paths.*/chipsmith/${backend}/${version}=${installRoot}/${backend}/${version}' /etc/nix/nix.conf")

        with subtest("a user installs the Toolchain where the module says it is"):
            builder.succeed("""su - alice -c '
              set -eu
              mkdir -p ~/.cache/chipsmith
              ${stageDownloads}
            '""")
            builder.succeed(
                "su - alice -c 'CHIPSMITH_INSTALL_ROOT=${installRoot} chipsmith install --backend ${backend} ${version}' >&2",
                timeout=3 * 3600,
            )
            builder.succeed("test -f ${installRoot}/${backend}/${version}/.chipsmith-nix-patch")

        with subtest("the derivation builds in the sandbox, on the Toolchain the module mounted"):
            out = builder.succeed("nix-build --no-out-link ${inVm}", timeout=3600).strip()
            builder.succeed(f"test -s {out}/${(builtins.fromTOML (builtins.readFile (project + "/chipsmith.toml"))).project.name}.sof")
            print(builder.succeed(f"ls -la {out}; cat {out}/*.sta.summary || true"))
      '';
    };

  # Altera's download center now sits behind a login, so the inputs come from
  # the Linux DVD image as archived on archive.org. Pinned to the SHA-1s Altera
  # publishes for the image and for each file in it, so neither the mirror nor
  # the extraction is taken on trust.
  quartusII13Dvd = pkgs.fetchurl {
    url = "https://archive.org/download/quartus-web-13.0.1.232_202410/Quartus-web-13.0.1.232.iso";
    hash = "sha1-NCERmwsIT6WXsGzGPfxPDPeiOHY=";
  };
  # archive.org can serve a single member of the image, but it streams it with
  # no length, so a dropped connection looks like a complete file.
  fromQuartusII13Dvd =
    name: hash:
    pkgs.runCommand name
      {
        nativeBuildInputs = [ pkgs.libarchive ];
        outputHash = hash;
        outputHashMode = "flat";
      }
      ''
        bsdtar -xOf ${quartusII13Dvd} components/${name} > $out
      '';

  # Quartus Prime Lite has no anonymous mirror: Altera's download center wants
  # a login. So this one is not a flake check. Download the two files from
  # https://www.altera.com/downloads/fpga-development-tools/quartus-prime-lite-edition-design-software-version-23-1-1-linux,
  # `nix-store --add-fixed sha1` each, then build
  # .#legacyPackages.x86_64-linux.e2e-quartus-prime. The SHA-1s are Altera's.
  # requireFile's contract, without requireFile's unfree marking: present in
  # the store under this hash, or fail saying how to put it there.
  quartusPrimeLite231 =
    name: hash:
    pkgs.runCommand name
      {
        outputHash = hash;
        outputHashMode = "flat";
      }
      ''
        echo "${name} is not in the store. Download it from" >&2
        echo "  https://www.altera.com/downloads/fpga-development-tools/quartus-prime-lite-edition-design-software-version-23-1-1-linux" >&2
        echo "then: nix-store --add-fixed sha1 ${name}" >&2
        exit 1
      '';
in
{
  checks = {
    package = chipsmith;

    e2e-quartus-ii-13 = e2e {
      backend = "quartus-ii-13";
      version = "13.0sp1";
      project = ../../example-de2;
      downloads = {
        "QuartusSetupWeb-13.0.1.232.run" =
          fromQuartusII13Dvd "QuartusSetupWeb-13.0.1.232.run" "sha1-FBiylxqkRobF0Rtsjvge7hNzeeg=";
        "cyclone_web-13.0.1.232.qdz" =
          fromQuartusII13Dvd "cyclone_web-13.0.1.232.qdz" "sha1-iCsZqP/uHt0TNpP/Qio/bFphVYk=";
        "max_web-13.0.1.232.qdz" =
          fromQuartusII13Dvd "max_web-13.0.1.232.qdz" "sha1-97MaoiNv99+i797ipMI/r+kfJ2Q=";
      };
    };
  };

  # Not yet run: written alongside the Quartus II one, never observed passing.
  manual.e2e-quartus-prime = e2e {
    backend = "quartus-prime";
    version = "23.1";
    project = ../../example;
    # The other Device Support packages 23.1 lists go undownloaded, which
    # chipsmith reports as a warning; example/ only needs Cyclone V.
    downloads = {
      "QuartusLiteSetup-23.1std.1.993-linux.run" =
        quartusPrimeLite231 "QuartusLiteSetup-23.1std.1.993-linux.run" "sha1-u8oJhsecpLNng4/KMbBh7Ye/5Q4=";
      "cyclonev-23.1std.1.993.qdz" =
        quartusPrimeLite231 "cyclonev-23.1std.1.993.qdz" "sha1-TYSVFurHUMleqlhI0iVz1g5cyiY=";
    };
    # Cyclone V fitting, and an install Altera sizes at 8.3 GB plus 1.4 GB
    memorySize = 12 * 1024;
    diskSize = 50 * 1024;
  };
}
