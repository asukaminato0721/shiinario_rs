# Third-party notices

The bundled `crates/na_mpeg2_decoder` library comes from `siglus_rs` revision
`91b0882ea938638c9eebe5e10e93c7f1be4fab61` under MPL-2.0. The decoder library
sources are unchanged; desktop examples and their dependencies are excluded.
See its [provenance](crates/na_mpeg2_decoder/README.md) and
[license](crates/na_mpeg2_decoder/LICENSE-MPL-2.0).

The WARC encryption/decompression and S25 decoder are Rust ports of GARbro
by morkt. The unchanged upstream format catalog is bundled as
[`crates/shiinario_assets/data/garbro/Formats.dat`](crates/shiinario_assets/data/garbro/Formats.dat).
The Rust catalog reader follows GARbro's serialized data layout to load the
 profile. [Catalog provenance and SHA-256](crates/shiinario_assets/data/garbro/README.md).
Source revision: b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0.
Repository: https://github.com/morkt/GARbro

```text
Copyright (C) 2015-2017 by morkt

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to
deal in the Software without restriction, including without limitation the
rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
IN THE SOFTWARE.
```

Other Cargo dependencies retain their own licenses. The bundled GARbro catalog
includes upstream decoder data, including the referenced ShiinaImage table.
