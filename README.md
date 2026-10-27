# shiinario_rs

An in-progress Rust compatibility runtime for Shiina Rio v2.36 and v2.47, with initial v2.48 and v2.49 support.

Other recognized v2.xx versions try the available parsers before any compatibility
diagnostic. The runtime retains the reported version. A parse failure includes
the original error and identifies an unverified version. Successful startup does
not establish complete gameplay support.

**Full-route gameplay is not complete yet but is mostly usable**

```
cargo b -r
```

Copy `target/release/shiinario_engine` into the game directory and run it.

Branch pushes build Linux, macOS and Windows binaries for x86_64 and ARM64,
plus FreeBSD x86_64. The GitHub Actions workflow uploads archives containing
`shiinario_engine` and `shiinario_tool` and refreshes the rolling `pre-release`
GitHub release after all builds succeed. Tag pushes do not run the workflow.

The runtime first tries to recover WARC decryption data from EXEs in the game
directory and its subdirectories. It does not require a specific basename, hash,
or file size. It recognizes
supported PE/code patterns, including the verified Crackproof byte transform,
without executing Windows code or loading `Formats.dat`. Directory symlinks are
not followed. If extraction fails, the bundled GARbro catalog matches EXE filenames
in the same directory tree and archive filenames in the game directory. See the
[recovery and catalog documentation](crates/shiinario_assets/data/garbro/README.md)
for supported patterns and limitations.

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

Native and browser hosts resume a shared main coroutine through `resume_main`.
The coroutine runs on `async-executor::LocalExecutor`. The host polls ready
futures on its own thread. Futures can retain non-`Send` state. A standard
`Waker` schedules a native event-loop wake or a browser worker tick.
The host retains a pending executor `tick()` future to receive queue wakes,
including task cancellation. Ready tasks use the executor's direct poll.
The coroutine advances the session and awaits timer waits or execution
budgets. It ends when the game ends. Each slice permits 4 ms or 10,000 events.
During a timer wait, the next script or movie deadline determines the next wake.
Early timer wakes do not execute the VM. Input wakes deliver window messages
before the deadline. Ready async tasks do not release a pending script timer.
VM task order and dispatch quantum remain explicit.
The browser also refills audio at intervals of at most 20 ms and schedules an
early wake when input arrives. Bytecode input loops remain runnable.
Scheduler regressions run with
`cargo test -p shiinario_runtime --test session_wait --test movie --locked`.
Browser worker and filesystem tests run with
`node --test platform/wasm/worker.test.mjs platform/wasm/vfs.test.mjs`.

Binary text output uses a coroutine built from stable Rust `async/await`.
The coroutine yields host requests and text scheduler ticks. The compiler
retains its execution position and local variables. Scoped operations access
VM state without keeping a VM borrow across a yield. Glyph replies commit
layout before execution resumes. The original task order remains explicit.
Regression tests compare 96 event traces with digests recorded before the
coroutine change, including skip input, clock wrap and image text.
Run them with `cargo test -p shiinario_scenario --test text_coroutine --locked`.

Graphics operations include image affine transforms, block pixelation,
image-frame text and portable drawing surfaces.
292 synthetic cases compare affine, pixelation and image-text pixels with the
original v2.47 kernels. Native font outlines still depend on installed fonts.
Unsupported commands report errors.

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
./shiinario_tool trace START.SCN --simulate-platform --tick-ms 1 --max-steps 20000000 --input /tmp/input.json --summary
```

Replay input is a JSON array of frames with strictly increasing `at_ms`, optional
logical `cursor: [x, y]`, `mouse_buttons` (left/right/middle bits), `keys` (Windows
virtual-key numbers), and `control_mask` (`256` holds the engine's skip control).
`scan_keys` supplies physical DirectInput scan-code indices for opcode `03ec`.
It is separate from virtual keys. For example, left Ctrl has scan code `29`.
Omit `cursor` in keyboard frames to preserve script pointer movements.
Opcode `0457` moves the engine cursor. Native windows also request a physical
pointer warp. The engine retains its logical position when the platform denies
that request. The next physical pointer event replaces this position.
The optional `--final-frame /tmp/new-frame.png` captures the last SCN display even
on failure and refuses to overwrite an existing file. State persists until the next frame. Exhausting the instruction budget is a failure,
not evidence that a story route ended.

For choice testing, the generator accepts `--cursor 400 190` (the first observed
option) and `--skip --release-skip-every 3`. Releasing skip between clicks allows
the first two menus to accept selection; holding it continuously leaves them
waiting. Input timestamps use elapsed u64 milliseconds and continue across the
engine's u32 clock wrap. `--summary --tail-events 2000` keeps recent events for
diagnosing a stalled replay.
Summary mode writes progress to stderr after each five million binary instructions.
Each update includes the glyph count, simulated elapsed time, and script position.
Add `--reload-input` to apply future input edits during a running replay.
Replace the JSON file atomically. Keep completed frames identical, and place
changed frames after the current simulated time. Invalid edits stop the replay.

Configured audio loops use decoded PCM frame boundaries. Native decoder block
effects at configured loop endpoints do not have sample-by-sample verification.
Changing a loop during active playback reports an error.

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
