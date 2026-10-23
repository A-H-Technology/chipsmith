# The 7-series tools chipsmith still shells out to, from openXC7's
# toolchain-nix (one consistent set: nextpnr, prjxray, fasm, prjxray-db).
#
# xpkgs is toolchain-nix's own nixpkgs: fasm is a CPython extension built
# against its python312, so the interpreter that imports it must be that one.
{
  lib,
  runCommand,
  writeShellScriptBin,
  xpkgs,
  openxc7,
}:

let
  inherit (openxc7) nextpnr prjxray prjxray-db fasm;

  # The dies chipsmith has a Device for (src/device.rs).
  dies = [ "xc7z010" ];

  # toolchain-nix's nextpnr-xilinx-chipdb.zynq7 builds all five Zynq dies
  # (about 20 minutes and GBs of RAM each for the large ones); chipsmith only
  # needs the Zybo's. Same commands as its nix/nextpnr-chipdb.nix.
  chipdb = runCommand "nextpnr-xilinx-chipdb-${lib.concatStringsSep "-" dies}" { } ''
    mkdir -p $out
    for die in ${lib.concatStringsSep " " dies}; do
      ${xpkgs.python3}/bin/python3 \
        ${nextpnr}/share/nextpnr/himbaechel/uarch/xilinx/gen/xilinx_gen.py \
        --xray ${prjxray-db}/zynq7 --device $die --bba $die.bba
      ${nextpnr}/bin/bbasm -l $die.bba $out/chipdb-$die.bin
      rm $die.bba
    done
  '';

  python = xpkgs.python312.withPackages (p: [
    fasm
    p.pyyaml
    p.simplejson
    p.intervaltree
    p.textx
    # fasm.parser imports pyximport (Cython) and, through it, distutils;
    # toolchain-nix's devshell puts the same two on PYTHONPATH.
    p.cython
    p.setuptools
  ]);

  # prjxray's python tools import its package from the source tree, which
  # toolchain-nix installs under usr/share/python3 rather than as a module.
  prjxrayPython = name: writeShellScriptBin name ''
    export PYTHONPATH=${prjxray}/usr/share/python3''${PYTHONPATH:+:$PYTHONPATH}
    exec ${python}/bin/python3 ${prjxray}/bin/${name} "$@"
  '';

  tools = runCommand "xc7-tools" { } ''
    mkdir -p $out/bin
    ln -s ${nextpnr}/bin/nextpnr-himbaechel $out/bin/
    ln -s ${prjxray}/bin/xc7frames2bit $out/bin/
    ln -s ${prjxray}/bin/bitread $out/bin/
    ln -s ${prjxrayPython "fasm2frames"}/bin/fasm2frames $out/bin/
    ln -s ${prjxrayPython "bit2fasm"}/bin/bit2fasm $out/bin/
  '';
in
{
  inherit chipdb tools prjxray-db;
}
