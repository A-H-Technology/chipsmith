# Legal questions

Where chipsmith touches vendor software and data, and why we think each
of those is allowed. This is our own reading, not legal advice, and no
lawyer has reviewed it. Written 2026-11-10. The project is developed in
the EU (Denmark), so EU law comes first; US law matters mainly because
GitHub hosts the repository.

## Summary

| Question | Assessment | Status |
|---|---|---|
| Black-box study of Quartus' and Vivado's output | Allowed | Nothing to do |
| Bitstream formats themselves | Facts, not copyrightable | Nothing to do |
| Cyclone II chip database extracted from Quartus' devinfo | The one real question: EU database right | Open, see below |
| Disassembly of Quartus' libraries | Allowed for interoperability; the EULA wants notice to Altera first | Notice not sent |
| Cell delays in `timing.rs` | Measured on the DE2 since 2026-11-10; the DMF values are gone | Done |
| Publishing benchmarks of Quartus | Forbidden by the EULA | Removed, 7ae43ff |
| Fetching the Quartus 13.0sp1 ISO from archive.org | Low risk | Unchanged |
| Third-party HDL in `examples/` | Licenses kept | Nothing to do |
| prjxray-db | ISC/CC0 | Nothing to do |
| Copyright holder | LICENSE names a person, repo sits under A-H-Technology | Open |

## The Quartus license

Quartus II 13.0sp1's terms are in `quartus/license.txt` of an install
(the "Quartus II, Version 13.0 Standard License Agreement", about line
200). Three clauses matter:

- **Reverse engineering.** Forbidden, except that a licensee in the EU
  "may perform limited reverse engineering, but only after giving notice
  to Altera and only to the extent permitted by the EC Software
  Directive". We haven't sent that notice. Sending one is cheap and
  closes off the contract argument.
- **Benchmarks.** "Licensee may not publish or disclose the results of
  any benchmarking of the software, or use such results for licensee's
  own competing software development activities, without the prior
  written permission of Altera." See "Benchmarks" below.
- **Devices.** The software may only program Altera devices. chipsmith
  doesn't use Quartus to program anything, so this doesn't apply.

The MegaCore Function agreement in the same file has its own benchmark
ban (section 3.3). chipsmith uses no MegaCore functions.

## Studying the tools from the outside

Most of the Cyclone II and all of the 7-series work treats the vendor
tool as a black box: build designs, compare the bitstreams, time them
with the vendor's STA. Software Directive 2009/24/EC Art. 5(3) lets
anyone entitled to use a program observe, study and test how it works
while using it, and Art. 8 voids contract terms against that. The US
equivalent is fair use (*Sega v. Accolade*) and DMCA §1201(f).

Which configuration bit does what is a fact about the silicon. That kind
of fact isn't copyrightable. IceStorm, Project X-Ray, Trellis, Apicula
and Mistral have published the same kind of data for years without any
vendor action.

## The Cyclone II chip database

cyclone2-re's `db/EP2C35F672C6/` (about 12 MB, committed) is what every
DE2 build reads:

- `chipdb.txt.xz`: the routing graph. 2.3M wires, 3.2M pips, 321k
  multi-tile nodes, the tile grid, the cells and their pins, and the
  package's pads.
- `device.bin.xz`: the same graph in Quartus' own form, the CRAM bits
  that enable each routing edge, Quartus' assembler tables (muxes, bit
  groups, defaults) and the user pins.

`cram db` extracts both from Quartus' devinfo files
(`crates/chipsmith-cyclone2/src/bitstream/db.rs`), and the file formats
were decoded with Ghidra and Quartus' own trace output
(cyclone2-re `docs/ddb-format.md`). So this isn't rediscovered data
like prjxray-db. It's Altera's internal database, translated.

- **Copyright:** weak. The content is facts about the chip.
- **Decompiling:** Art. 6 of the Software Directive allows it to get the
  information an independent program needs to interoperate, when that
  information isn't otherwise available. A bitstream generator for the
  vendor's chip fits that closely. Altera publishes no bitstream
  documentation.
- **Database right (Directive 96/9/EC):** this is the real exposure.
  The right protects a database whatever its content, and wholesale
  extraction is what it's aimed at. The interoperability exceptions
  above don't cover it. The counter-argument is *British Horseracing
  Board v William Hill* (C-203/02): the right only protects investment
  in *obtaining and verifying* existing data, not in *creating* it.
  Altera created this data by designing the chip. That argument is good
  but untested for FPGA data.

The lower-risk alternative is to ship `cram db` instead of its output,
so each user extracts the database from their own Quartus install. That
costs every user a Quartus download. We haven't done it.

## Disassembly

cyclone2-re's docs cite functions in Quartus' `.so` files and read some
facts "in disassembly". That's decompilation in the Directive's sense.
Art. 6 allows it on the conditions above, plus: only the parts needed,
and the information isn't used for anything else. The EULA's notice
requirement (above) is the remaining contract question.

## Timing values

`crates/chipsmith-route/src/timing.rs` used to hardcode a few dozen cell
delays decoded from Quartus' DMF files. Decoding a vendor database file
and copying its values is the kind of extraction the EU database right is
about, however few the values, and quartus_sta reports the DMF values
verbatim, so reading them off its reports would only copy the same data
by another path.

Since 2026-11-10 (docs/timing-silicon.md) no value in the timing model
comes from the DMF files:

- **Measured.** The LUT, carry-chain and multiplier delays come from
  ring oscillators on our own DE2.
- **Scaled.** The slow-corner scale per class of cell, and the register,
  M4K and I/O-cell constants, are least-squares fits to quartus_sta's
  path totals on placements chipsmith made. That's observation of a
  program's output (Software Directive Art. 5(3); CJEU *SAS Institute v
  World Programming*, C-406/10), in aggregate, against chipsmith's own
  routing model. No per-cell number is read off a report.

The routing delays are quartus_sta's median per element type over
chipsmith's designs, which is the same kind of observation.

The old values are still in the git history.

## Benchmarks

The README and `docs/vs-quartus.md` published LE counts, build times and
Fmax for Quartus next to chipsmith's. Since 7ae43ff they don't: the README
says chipsmith is much faster without numbers, `docs/vs-quartus.md` is
gone, and `bench/vs-quartus.sh` is marked for local use. Using Quartus
as a measuring stick for chipsmith (byte-identical bitstream checks,
quartus_sta on chipsmith's placements) stays. Those measure chipsmith,
not Quartus.

- The old numbers are still in the public git history. Removing them
  would mean rewriting `main`. We haven't.
- The clause also forbids using benchmark results for competing
  development, which is how branches `qor` to `qor5` were done. In the
  EU, Art. 8 arguably voids that, since it restricts observing and
  studying the program.
- A clause like this binds only whoever accepted the EULA, and the
  realistic enforcement is a takedown request.

Don't commit `bench/vs-quartus.*` output, or any other table of Quartus'
results.

## The Quartus installer

Altera's download center needs a login since 2026-10, so the e2e tests
fetch the Quartus II 13.0sp1 DVD image from archive.org
(`nix/vendor/tests/default.nix`), pinned to Altera's own SHA-1s.

- The repository hosts nothing. It names a URL and a hash.
- Under *GS Media* (C-160/15), linking to an unauthorized copy can
  infringe when the linker knows it's unauthorized, and knowledge is
  presumed when the link is made for profit. The comment in the nix file
  shows we know Altera doesn't publish it there.
- Quartus 13.0 Web Edition is free and long abandoned, so practically
  the risk is archive.org removing the upload. That would break the
  flake check, not us legally.
- Quartus Prime Lite 23.1 already uses a manual
  `nix-store --add-fixed` step. Doing the same for 13.0sp1 removes the
  question entirely. Do it if chipsmith is ever sold.

**Never push Quartus to a public binary cache.** The extracted files in
the nix store are Altera's software. Pushing them would be
redistribution, which is a clear line, unlike the link. There's no cache
setup today, and the VM test sets `substituters = [ ]`.

## Circumvention

InfoSoc Directive 2001/29 Art. 6 (ophavsretsloven §75c in Denmark),
Software Directive Art. 7(1)(c) and DMCA §1201 only apply when a
technical protection is bypassed: a license check, a dongle, bitstream
encryption. Nothing in chipsmith or cyclone2-re does that. Cyclone II
has no bitstream encryption, and as far as we know Web Edition needs no
license file for it. Keep it that way.

## Hosting

GitHub is in the US. A DMCA notice can get the repository disabled while
it's disputed, whatever EU law says. GitHub has a counter-notice process
and has sided with reverse-engineering projects before (youtube-dl,
2020). A mirror outside GitHub is cheap insurance.

## Patents

Vendor patents mostly cover the silicon, and the Cyclone II generation's
(2004) have expired. There may be live patents on packing or placement
methods. Every open FPGA tool carries that same unknown, and none has
been sued over it. Worth a search only if chipsmith is sold.

## Third-party code

- picorv32 (ISC, header in the file), sha256 (BSD), femtorv32, serv,
  crc32, uart, spi-master and i2c-master each keep their license in
  their example. The Zybo and `picorv32x24` variants point at those
  sources instead of copying them.
- prjxray-db is ISC/CC0. yosys and nextpnr are ISC. The packer was
  ported from cyclone2-re's own nextpnr uarch.
- GHDL is GPL but runs as a separate program, so its license doesn't
  extend to chipsmith.
- Naming Quartus, Cyclone, Zynq and Vivado is nominative use. Don't use
  their logos or suggest endorsement.

## Copyright holder

`LICENSE` says "Copyright (c) 2026 Alexandra Østermark", while the
repository and flake live under `A-H-Technology`. If A-H-Technology is
the owner, make the two match (an assignment or an updated notice). If
it's an employer, check the employment contract's IP clause.

## If chipsmith goes commercial

Ask an IP lawyer about the chip database (database right, *BHB*). Then
switch the Quartus 13.0sp1 fetch to a manual step, send Altera the
reverse-engineering notice, and settle the copyright holder.
