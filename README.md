# shiinario_rs

An in-progress Rust compatibility runtime for Ran→Sem (Shiina Rio v2.47).
**This is not yet a playable port.** Asset decoding is verified against GARbro;
binary scenario execution, native presentation, menus, choices, movies, and saves
remain unfinished. The engine currently reports the first unsupported startup
opcode and exits with an error. It does not skip startup or pretend to run it.

The workspace builds independently of the reference checkouts:

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Built and tested with Rust 1.98.1 on Linux. An older minimum toolchain has not been verified.

## Commands

```sh
# Read original configuration and attempt the real startup (executes 0x049d; stops at 0x000a, byte 7).
target/release/shiinario_engine --project-dir /path/to/game

# Inspect CP932 configuration and list the original archive entries.
target/release/shiinario_tool inspect --project-dir /path/to/game
target/release/shiinario_tool list /path/to/game/RAN_T0.WAR

# Decode every entry; --media additionally decodes all S25 frames and Vorbis packets.
target/release/shiinario_tool verify --project-dir /path/to/game --media

# Extract one entry to an explicit output path, or convert an image/audio asset.
target/release/shiinario_tool extract /path/to/game/RAN_T0.WAR START.SCN /tmp/start.scn
target/release/shiinario_tool image /path/to/game/RAN_BG.WAR BG01A.S25 --frame 0 /tmp/bg.png
target/release/shiinario_tool audio /path/to/game/RAN_D0.WAR SEA.OGV /tmp/se.ogg

# Dump text records with original byte offsets; binary dumps show candidate strings only.
target/release/shiinario_tool dump --project-dir /path/to/game A001.TXT
target/release/shiinario_tool dump --project-dir /path/to/game START.SCN
target/release/shiinario_tool inventory --project-dir /path/to/game

# Research traces for the verified SCN/TXT subsets. Unknown operations stop execution.
target/release/shiinario_tool trace --project-dir /path/to/game START.SCN --max-steps 10000
target/release/shiinario_tool trace --project-dir /path/to/game A001.TXT --max-steps 10000
```

Extraction writes only to the explicitly provided output path. Reading, inspecting,
verifying, and tracing do not write to the installation. Keep extracted assets and
research artifacts outside this repository. No alternative save format is created.

## Structure

- `shiinario_assets`: WARC 1.7 index and entry decoding, YH1/YPK/YLZ, S25 frames,
  OGV/Vorbis, project configuration, Windows-style path lookup, bounded byte cache.
- `shiinario_scenario`: CP932 story parser, command inventory, bytecode research
  dumps, typed presentation events and deterministic input/timing for a limited
  text interpreter and the verified binary mouse-button mapping opcode. Unsupported
  commands produce sticky errors with source context.
- `shiinario_runtime`: host-independent startup and trace orchestration.
- `shiinario_cli`: the engine and inspection tools. No native window backend yet.

See [compatibility and validation](docs/COMPATIBILITY.md) for exact coverage and
remaining work, [profile documentation](crates/shiinario_assets/profiles/ransem-v1/README.md)
for reproducible profile export, and [third-party notices](THIRD_PARTY_NOTICES.md).

See [SCN reverse-engineering notes](docs/SCN_RESEARCH.md) for the captured engine,
Ghidra workflow and original-machine-code verification of opcode `0x049d`.
