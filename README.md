# shiinario_rs

An in-progress Rust compatibility runtime for Shiina Rio v2.36 and v2.47.
**Full-route gameplay is not complete yet.** The Linux winit/wgpu host displays
an 800×600 title screen, plays music and effects through CPAL, and enters the story
with Japanese text and transitions. The title pixels match the original x86 renderer.
Native and headless hosts share the same interpreter and resources. Native text uses
Fontconfig and an installed Japanese font; the fallback glyphs differ from Windows.
MPEG-1/2 movies play with audio, including pause, resume, seek and loop controls.
Complete menu/choice coverage and compatible saves remain unfinished.

Native playback defaults to best effort: identified optional presentation operations
print `SKIP` with their scenario location. Currently this approximates image-frame text, omits affine/pixelation filters,
applies audio fades immediately and uses software buffers for DirectDraw surfaces.
The DirectShow COM probe reports unavailable; scripts use the implemented legacy
MPEG movie path. Movie playback also works in strict mode. Other movie formats
are rejected with an error. Calendar metadata is a fixed placeholder;
automatic and manual file writes are logged and omitted, so no save is created. Use `--strict` to reject these approximations.
Other unknown commands still stop with context because their operands and control-flow
effects are not known. Headless traces require explicit `--best-effort` for these omissions.
Assets are verified against GARbro and original game files remain unchanged.

Wana (`wana.EXE`, v2.36) is recognized using its bundled GARbro scheme. A shared
`EngineVersion` enum dispatches archive cryptography, program information, and
version-dependent SCN operands. Its archives decode, the title accepts Start,
and the first story runs in deterministic replay. Full-route coverage is unverified.

The workspace embeds a pinned upstream [GARbro format catalog](crates/shiinario_assets/data/garbro/README.md)
and reads it directly in Rust. Builds need no external database or reference
checkout; running the engine only requires the original game directory.

The native window uses the original game EXE's icon (read as PE resources, never
executed). Winit is pinned to `0.31.0-beta.3` for Wayland's
`xdg_toplevel_icon_v1` support. Compositors without that protocol keep their default
icon. X11 uses the same extracted pixels. Pointer coordinates follow the scaled,
letterboxed canvas regardless of the legacy Windows mouse-adjustment settings.

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

## Commands

Copy `target/release/shiinario_engine` into the game directory, open a terminal there, and
run `./shiinario_engine`. The current working directory is the game root. Registry string
queries use that root for every key; integer queries return zero. No system
registry is accessed. Keep the original EXE filenames: the engine uses GARbro
`GameMap` to select decryption data from filenames in this directory. It does not
execute the EXEs. Unknown or ambiguous matches produce an error.

For the research commands below, also copy `target/release/shiinario_tool` into
the game directory. Run these commands from that directory:

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
