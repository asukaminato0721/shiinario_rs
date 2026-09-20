# Executable recovery and GARbro fallback

The default archive/project loaders first try static recovery from the original
game EXEs beside the archives. This path does not read `Formats.dat`, use a
memory dump, launch Wine, or execute/emulate x86 code. It also identifies the EXE
used for icon lookup. The EXE basename may change; its `.exe` extension is needed
for directory discovery.

Recovery has no game-name dispatch, executable hash whitelist, or exact file-size
check. It reads PE32 address mappings and looks for the ShiinaRio version banner,
key template, date-copy instructions, helper call, image pointer/length constant,
and region PNG reference. Addresses come from those instructions, so section
file offsets and the image base can change. Candidates must supply a consistent
set of supported patterns; a key-like string alone is insufficient.

The loader first tries the ordinary PE contents. For the supported single-section
Crackproof byte-transform layout it also tries decoding the range between the
section table and resource directory, then applies the same extraction logic.
This is a static transform, not a general-purpose unpacker, and does not create
a runnable unpacked EXE. The embedded v2.47 `DecodeBin` is located through its
initialization call and rebuilt from both Huffman layers, including runtime tree
and bit-counter state.

Currently the decryption algorithms and instruction patterns support v2.36 and
v2.47. Other compiler layouts, engine versions or packer transforms can fail
extraction even when the game uses WARC. The supplied v2.36 archives never use the
second stage, so that recovered profile has no `DecodeBin`; entries requiring it
report an error. The original optional external `decode.bin` override is not
implemented.

Directory discovery tries every `.exe` (case-insensitive, up to 128 MiB), including
renamed or patched files. Unsupported candidates are skipped. If none can be
extracted, the existing catalog filename detection described below is used.
Multiple EXEs with identical recovered profiles are accepted; different profiles
produce an ambiguity error. Direct `Catalog` APIs still explicitly use the
catalog; `Profile::from_executable` explicitly requires recovery.

Tests use synthetic PEs with different raw offsets and image bases, changed key
data, unrelated EXEs and conflicting profiles. The original-game regression
compares every archive entry against the catalog path, then checks detection
with arbitrary EXE names, modified timestamps and appended overlays. No original
game bytes are committed.
Run it locally with paths to the original EXEs:

```sh
SHIINARIO_WANA_EXE='/path/to/wana.EXE' \
SHIINARIO_RAN_EXE='/path/to/RANDL_.exe' \
cargo test -p shiinario_assets --release --locked \
  original_games_match_catalog_for_every_entry -- --ignored --nocapture
```

Both supplied installations passed the full 20-archive comparison. The Ran→Sem
runtime table was also checked by executing its original initialization code in
Unicorn during research; its SHA-256 is
`79ac02929638cc345967a9b688c2aef3e96c6123fbb96c8771f0917a997f95c2`.

## Bundled fallback catalog

`Formats.dat` is an unchanged copy of GARbro's `ArcFormats/Resources/Formats.dat`:

- Upstream: [morkt/GARbro, pinned source file](https://github.com/morkt/GARbro/blob/b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0/ArcFormats/Resources/Formats.dat).
- Revision: `b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0`.
- Database version: 148.
- SHA-256: `54039fde222592c911536b0bd3f4d931bd580c66afd96eaefece34ea872f2982`.
- License: MIT; see [third-party notices](../../../../THIRD_PARTY_NOTICES.md).

The Rust build embeds this file. On first catalog use, the Rust reader decompresses its
NRBF records and loads the WARC schemes and `GameMap` filename mappings. It reads
data only and does not instantiate .NET classes. Validated profiles are cached
and shared between archives; other formats are discarded. No external catalog, Python, .NET runtime,
or GARbro installation is required to build or run the engine.

In the fallback path, the game directory is identified from its original `.exe` and `.war` filenames
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
