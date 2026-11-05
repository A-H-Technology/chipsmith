# Chip databases are data from the RE repos; the bitstream code lives in chipsmith

The Cyclone II chip database (cyclone2-re's `db/`, `chipdb.txt.xz`) and the 7-series one (prjxray-db) are inputs chipsmith reads, the way any toolchain reads a device database. They're produced and validated in their reverse-engineering repos, against the vendor tools, and reach chipsmith through nix. That is data, not shelling out.

The code that turns a routed design into a bitstream is not data, so it moves into chipsmith: cram's assembler (`cram pack`, `cram svf`) becomes `chipsmith-cyclone2`'s bitstream module, called in process. cyclone2-re's `cram` CLI then depends on that crate instead of owning a second copy, and its byte-identical checks against Quartus keep guarding the one implementation.

## Considered options

- Keep running `cram` as a binary: one more external program in the flow, which is what this work removes.
- Copy cram's code into chipsmith and leave cyclone2-re's copy: two assemblers that will drift, with only one of them checked against Quartus.
