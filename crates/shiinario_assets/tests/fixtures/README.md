`minimal.war` is a synthetic WARC 1.7 archive, not a game asset. Its only entry is
`fixture.txt`, containing the ASCII bytes `synthetic asset fixture\n`, wrapped in
YPK/zlib. The 56-byte index was zlib-compressed, padded to GARbro's maximum index
length, encrypted with the original GARbro `Decoder.EncryptIndex` using the bundled
Ran→Sem profile, and stored with 1024 index bytes. Index offset is 52. This tests
compatibility with an independent writer rather than a Rust round-trip encoder.
