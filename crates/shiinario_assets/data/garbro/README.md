# GARbro format catalog

`Formats.dat` is an unchanged copy of GARbro's `ArcFormats/Resources/Formats.dat`:

- Upstream: [morkt/GARbro, pinned source file](https://github.com/morkt/GARbro/blob/b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0/ArcFormats/Resources/Formats.dat).
- Revision: `b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0`.
- Database version: 148.
- SHA-256: `54039fde222592c911536b0bd3f4d931bd580c66afd96eaefece34ea872f2982`.
- License: MIT; see [third-party notices](../../../../THIRD_PARTY_NOTICES.md).

The Rust build embeds this file. On first use, the Rust reader decompresses its
NRBF records and selects the  v2.47 scheme. It reads data only and does not
instantiate .NET classes. The validated profile is shared between archives; the
other catalog records are discarded. No external catalog, Python, .NET runtime,
or GARbro installation is required to build or run the engine.

The full upstream catalog does not imply support for other games. The reader
validates the selected scheme version, entry name size and supported crypt stages.

To update, copy the original file from an explicitly selected upstream revision,
record its revision/version/hash here and in
[`profile.json`](../../profiles/ransem-v1/profile.json), and independently compare
the selected table hashes with GARbro before updating the recorded expectations.
Run the workspace tests and the original-game reference comparison described in
[`COMPATIBILITY.md`](../../../../docs/COMPATIBILITY.md). Do not regenerate or edit
the serialized database locally.
