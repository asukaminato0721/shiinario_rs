# shiinario_rs

An in-progress Rust compatibility runtime for Ran→Sem (Shiina Rio v2.47).
**Full-route gameplay is not complete yet.** The Linux winit/wgpu host displays
an 800×600 title screen, plays music and effects through CPAL, and enters the story
with Japanese text and transitions. The title pixels match the original x86 renderer.
Native and headless hosts share the same interpreter and resources. Native text uses
Fontconfig and an installed Japanese font; the fallback glyphs differ from Windows.
Movies, complete menu/choice coverage and compatible saves remain unfinished.

Native playback defaults to best effort: identified optional presentation operations
print `SKIP` with their scenario location. Currently this approximates image-frame text, omits affine/pixelation filters,
applies audio fades immediately and uses software buffers for DirectDraw surfaces.
The movie capability probe reports unavailable; the reached movie path is skipped
and reports completion. Actual movie playback is not implemented. Use `--strict` to reject these approximations.
Other unknown commands still stop with context because their operands and control-flow
effects are not known. Headless traces require explicit `--best-effort` for these omissions.
Assets are verified against GARbro and original game files remain unchanged.

The workspace builds independently of the reference checkouts. On Linux, CPAL
requires ALSA development headers and pkg-config:

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Built and tested with Rust 1.98.1 on Linux. An older minimum toolchain has not been verified.

## Commands

```sh
# Open a native window and run the original configured startup.
target/release/shiinario_engine --project-dir /path/to/game
# Optional bounded native startup run.
target/release/shiinario_engine --project-dir /path/to/game --run-for-ms 18000
# Export runtime-modified SCNs for research; the destination must be new.
target/release/shiinario_engine --project-dir /path/to/game --scenario-dump /tmp/rio-scn-dump

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

# Research traces for the recovered SCN/TXT subsets.
target/release/shiinario_tool trace --project-dir /path/to/game START.SCN --max-steps 10000
# Explicit simulated Windows replies, logged in the output; this does not display a game window.
target/release/shiinario_tool trace --project-dir /path/to/game START.SCN --simulate-platform --tick-ms 16 --max-steps 40000
target/release/shiinario_tool trace --project-dir /path/to/game A001.TXT --max-steps 10000
# Generate repeatable Start/advance input (does not select choices).
python tools/research/make_story_input.py /tmp/input.json --skip
# Deterministic input replay with compact counts and explicit compatibility omissions.
target/release/shiinario_tool trace --project-dir /path/to/game START.SCN --simulate-platform --tick-ms 1 --max-steps 20000000 --input /tmp/input.json --best-effort --summary
```

Replay input is a JSON array of frames with strictly increasing `at_ms`, logical
`cursor: [x, y]`, optional `mouse_buttons` (left/right/middle bits), `keys` (Windows
virtual-key numbers), and `control_mask` (`256` holds the engine's skip control).
State persists until the next frame. Exhausting the instruction budget is a failure,
not evidence that a story route ended.

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
remaining work, [profile documentation](crates/shiinario_assets/profiles/ransem-v1/README.md)
for reproducible profile export, and [third-party notices](THIRD_PARTY_NOTICES.md).

See [SCN reverse-engineering notes](docs/SCN_RESEARCH.md) for the captured engine,
Ghidra workflow and original-machine-code verification of opcode `0x049d`.
