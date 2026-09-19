# GARbro format catalog

`Formats.dat` is an unchanged copy of GARbro's `ArcFormats/Resources/Formats.dat`:

- Upstream: [morkt/GARbro, pinned source file](https://github.com/morkt/GARbro/blob/b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0/ArcFormats/Resources/Formats.dat).
- Revision: `b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0`.
- Database version: 148.
- SHA-256: `54039fde222592c911536b0bd3f4d931bd580c66afd96eaefece34ea872f2982`.
- License: MIT; see [third-party notices](../../../../THIRD_PARTY_NOTICES.md).

The Rust build embeds this file. On first use, the Rust reader decompresses its
NRBF records and loads the WARC schemes and `GameMap` filename mappings. It reads
data only and does not instantiate .NET classes. Validated profiles are cached
and shared between archives; other formats are discarded. No external catalog, Python, .NET runtime,
or GARbro installation is required to build or run the engine.

The game directory is identified from its original `.exe` and `.war` filenames
using `GameMap`, without reading or executing the EXEs. Direct archive access
checks the archive filename first, then EXEs beside it, as GARbro does. Matching
is case-insensitive. Missing or ambiguous matches produce an error. Keep the
original filenames; there is no fallback game. One explicit mapping correction
connects `wana.exe` to the existing `Wana ~Hakudaku Mamire no Houkago~` scheme,
which the pinned catalog contains without a corresponding `GameMap` entry.
The serialized upstream catalog remains unchanged.

The full upstream catalog does not imply complete support for other games. The
reader currently accepts v2.36/v2.47 schemes with 16- or 32-byte entry names and no
extra crypt stage, and validates table sizes. An enum selects the version-specific
helper transform; index size follows the scheme's entry-name size.
Unsupported mapped schemes report their name
and validation error. Interpreter compatibility is a separate constraint.

To update, copy the original file from an explicitly selected upstream revision,
record its revision/version/hash here, and independently compare the selected
table hashes with GARbro before updating the expectations in
[`profile.rs`](../../src/profile.rs).
Run the workspace tests and the original-game reference comparison described in
[`COMPATIBILITY.md`](../../../../docs/COMPATIBILITY.md). Do not regenerate or edit
the serialized database locally.
