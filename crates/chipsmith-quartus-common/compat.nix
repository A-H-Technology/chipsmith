# The store paths an FHS-assuming vendor binary needs on a nix host.
#
# One list, read two ways: a nix-built chipsmith gets it baked in at build time
# (see flake.nix), and a `cargo install`ed one evaluates it against <nixpkgs>
# at run time (see src/nixos.rs). Keep it a function of `pkgs` alone.
{
  pkgs,
  # Naming pkgsi686Linux forces nix to instantiate the whole i686 package set,
  # which is seconds of evaluation a 64-bit-only Product should not pay at run
  # time. The baked-in set always includes it, since evaluation happens once.
  i686 ? true,
}:
{
  glibc = pkgs.glibc.outPath;
  gcc-lib = pkgs.gcc-unwrapped.lib.outPath;
  zlib = pkgs.zlib.outPath;
  patchelf = pkgs.patchelf.outPath;
  bash = pkgs.bash.outPath;
  ncurses = pkgs.ncurses.outPath;
  freetype = pkgs.freetype.outPath;
  fontconfig = pkgs.fontconfig.lib.outPath;
  # nixpkgs moved the xorg set to top-level lowercase names; `or` keeps both
  # old channels and new ones evaluating, and new ones without warnings.
  libxrender = (pkgs.libxrender or pkgs.xorg.libXrender).outPath;
  libxext = (pkgs.libxext or pkgs.xorg.libXext).outPath;
  libx11 = (pkgs.libx11 or pkgs.xorg.libX11).outPath;
  libxi = (pkgs.libxi or pkgs.xorg.libXi).outPath;
  libxtst = (pkgs.libxtst or pkgs.xorg.libXtst).outPath;
  libxft = (pkgs.libxft or pkgs.xorg.libXft).outPath;
  dbus = pkgs.dbus.lib.outPath;
  glib = pkgs.glib.out.outPath;
  libpng = pkgs.libpng.outPath;
  expat = pkgs.expat.outPath;
  libxml2 = pkgs.libxml2.outPath;
  libxcb = (pkgs.libxcb or pkgs.xorg.libxcb).outPath;
  libxau = (pkgs.libxau or pkgs.xorg.libXau).outPath;
  libxdmcp = (pkgs.libxdmcp or pkgs.xorg.libXdmcp).outPath;
  libxcrypt-legacy = pkgs.libxcrypt-legacy.outPath;
  libsm = (pkgs.libsm or pkgs.xorg.libSM).outPath;
  libice = (pkgs.libice or pkgs.xorg.libICE).outPath;
  krb5 = pkgs.krb5.lib.outPath;
  bzip2 = pkgs.bzip2.out.outPath;
  systemd = pkgs.systemd.outPath;
  libxfixes = (pkgs.libxfixes or pkgs.xorg.libXfixes).outPath;
  libxdamage = (pkgs.libxdamage or pkgs.xorg.libXdamage).outPath;
  libxcomposite = (pkgs.libxcomposite or pkgs.xorg.libXcomposite).outPath;
  libxrandr = (pkgs.libxrandr or pkgs.xorg.libXrandr).outPath;
  libxcursor = (pkgs.libxcursor or pkgs.xorg.libXcursor).outPath;
  libxinerama = (pkgs.libxinerama or pkgs.xorg.libXinerama).outPath;
  libuuid = pkgs.util-linux.lib.outPath;
}
// pkgs.lib.optionalAttrs i686 {
  glibc32 = pkgs.pkgsi686Linux.glibc.outPath;
  gcc-lib32 = pkgs.pkgsi686Linux.gcc-unwrapped.lib.outPath;
  zlib32 = pkgs.pkgsi686Linux.zlib.outPath;
  bubblewrap = pkgs.bubblewrap.outPath;
}
