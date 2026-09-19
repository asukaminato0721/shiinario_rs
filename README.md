# shiinario_rs

An in-progress Rust compatibility runtime for Shiina Rio v2.36 and v2.47.

**Full-route gameplay is not complete yet but is mostly usable**

```
cargo b -r
```

Copy `target/release/shiinario_engine` into the game directory and run it.

you may need to fix the launch program since the key maybe not set

(ask your agent for this)

---


Development

On Linux, CPAL requires ALSA development headers and pkg-config:

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Movie decoding uses the bundled MPEG decoder from `siglus_rs`; neither that
checkout nor FFmpeg is needed at runtime. Video frames are decoded incrementally,
while compressed input and decoded audio are retained under a memory budget.
The independent movie regression suite runs with
`cargo test -p shiinario_runtime --test movie --locked` and uses a synthetic clip.

Built and tested with Rust 1.98.1 on Linux. An older minimum toolchain has not been verified.

```sh
# Open a native window and run the original configured startup.
./shiinario_engine
# Optional bounded native startup run.
./shiinario_engine --run-for-ms 18000
# Export runtime-modified SCNs for research; the destination must be new.
./shiinario_engine --scenario-dump /tmp/rio-scn-dump

# Inspect CP932 configuration and list the original archive entries.
./shiinario_tool inspect
# Export the original EXE icon; the output file must be new.
./shiinario_tool icon /tmp/game-icon.png
./shiinario_tool list RAN_T0.WAR

# Decode every entry; --media additionally decodes all S25 frames and Vorbis packets.
./shiinario_tool verify --media

# Extract one entry to an explicit output path, or convert an image/audio asset.
./shiinario_tool extract RAN_T0.WAR START.SCN /tmp/start.scn
./shiinario_tool image RAN_BG.WAR BG01A.S25 --frame 0 /tmp/bg.png
./shiinario_tool audio RAN_D0.WAR SEA.OGV /tmp/se.ogg

# Dump text records with original byte offsets; binary dumps show candidate strings only.
./shiinario_tool dump A001.TXT
./shiinario_tool dump START.SCN
./shiinario_tool inventory

# Research traces for the recovered SCN/TXT subsets.
./shiinario_tool trace START.SCN --max-steps 10000
# Explicit simulated Windows replies, logged in the output; this does not display a game window.
./shiinario_tool trace START.SCN --simulate-platform --tick-ms 16 --max-steps 40000
./shiinario_tool trace A001.TXT --max-steps 10000
# Generate repeatable Start/advance input (does not select choices).
python tools/research/make_story_input.py /tmp/input.json --skip
# Deterministic input replay with compact counts and explicit compatibility omissions.
./shiinario_tool trace START.SCN --simulate-platform --tick-ms 1 --max-steps 20000000 --input /tmp/input.json --best-effort --summary
```

Replay input is a JSON array of frames with strictly increasing `at_ms`, logical
`cursor: [x, y]`, optional `mouse_buttons` (left/right/middle bits), `keys` (Windows
virtual-key numbers), and `control_mask` (`256` holds the engine's skip control).
The optional `--final-frame /tmp/new-frame.png` captures the last SCN display even
on failure and refuses to overwrite an existing file. State persists until the next frame. Exhausting the instruction budget is a failure,
not evidence that a story route ended.

For choice testing, the generator accepts `--cursor 400 190` (the first observed
option) and `--skip --release-skip-every 3`. Releasing skip between clicks allows
the first two menus to accept selection; holding it continuously leaves them
waiting. Input timestamps use elapsed u64 milliseconds and continue across the
engine's u32 clock wrap. `--summary --tail-events 2000` keeps recent events for
diagnosing a stalled replay.

Extraction writes only to the explicitly provided output path. Reading, inspecting,
verifying, and tracing do not write to the installation. Keep extracted assets and
research artifacts outside this repository. No alternative save format is created.

## Structure

- `shiinario_assets`: WARC 1.7 index and entry decoding, YH1/YPK/YLZ, S25 frames,
  OGV/Vorbis, project configuration, Windows-style path lookup, bounded byte cache.
- `shiinario_scenario`: CP932 story parser, command inventory, bytecode research
  dumps, typed presentation events and deterministic input/timing for a limited
  text interpreter and the recovered binary startup subset: operand banks, bounded
  memory, task-local scopes and stacks, branches, calls, strings, cooperative scheduling,
  and platform requests.
  Unsupported commands produce sticky errors with source context.
- `shiinario_runtime`: shared session and host interface, bounded drawing buffers,
  image/audio resources, synthetic traces, winit/wgpu window presentation and CPAL.
- `shiinario_cli`: the native engine and inspection tools.

See [compatibility and validation](docs/COMPATIBILITY.md) for exact coverage and
remaining work, [catalog documentation](crates/shiinario_assets/data/garbro/README.md)
for provenance and profile validation, and [third-party notices](THIRD_PARTY_NOTICES.md).

See [SCN reverse-engineering notes](docs/SCN_RESEARCH.md) for the captured engine,
Ghidra workflow and original-machine-code verification of opcode `0x049d`.
