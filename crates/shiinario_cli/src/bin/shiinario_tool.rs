use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use shiinario_assets::Archive;
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    List {
        archive: PathBuf,
    },
    Dump {
        #[arg(long)]
        project_dir: PathBuf,
        name: String,
    },
    Inventory {
        #[arg(long)]
        project_dir: PathBuf,
    },
    Trace {
        #[arg(long)]
        project_dir: PathBuf,
        name: String,
        #[arg(long, default_value_t = 10000)]
        max_steps: usize,
        /// Use logged, synthetic window replies for interpreter research.
        #[arg(long)]
        simulate_platform: bool,
        /// Milliseconds per host poll; timer retries advance 1 ms. Zero freezes time.
        #[arg(long, default_value_t = 0, requires = "simulate_platform")]
        tick_ms: u32,
        /// JSON array of timestamped mouse/key snapshots for deterministic replay.
        #[arg(long, requires = "simulate_platform")]
        input: Option<PathBuf>,
        /// Log identified presentation omissions and continue.
        #[arg(long)]
        best_effort: bool,
        /// Print coverage totals instead of every interpreter event.
        #[arg(long)]
        summary: bool,
        /// Write the last binary-replay display to a new PNG file, even on failure.
        #[arg(long)]
        final_frame: Option<PathBuf>,
        /// Include up to 10000 recent events in the summary for stalled-input diagnosis.
        #[arg(long, default_value_t = 0, requires = "summary")]
        tail_events: usize,
    },
    Extract {
        archive: PathBuf,
        name: String,
        output: PathBuf,
    },
    Verify {
        #[arg(long)]
        project_dir: PathBuf,
        #[arg(long)]
        media: bool,
    },
    Inspect {
        #[arg(long)]
        project_dir: PathBuf,
    },
    Image {
        archive: PathBuf,
        name: String,
        #[arg(long, default_value_t = 0)]
        frame: usize,
        output: PathBuf,
    },
    Audio {
        archive: PathBuf,
        name: String,
        output: PathBuf,
        /// Decode to interleaved signed 16-bit little-endian PCM; print stream metadata.
        #[arg(long)]
        pcm: bool,
    },
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::Dump { project_dir, name } => {
            let p = shiinario_runtime::open(project_dir)?;
            let data = p.read(&name)?;
            if name.to_ascii_lowercase().ends_with(".txt") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&shiinario_scenario::TextScript::parse(
                        name, &data
                    )?)?
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"format":"binary_scn","size":data.len(),"note":"Candidate strings only; instruction boundaries are unresolved", "first_opcode":data.get(..2).map(|b|u16::from_le_bytes([b[0],b[1]])),"string_candidates":shiinario_scenario::binary_strings(&data)})
                    )?
                );
            }
        }
        Command::Inventory { project_dir } => {
            let p = shiinario_runtime::open(project_dir)?;
            let mut commands = std::collections::BTreeMap::<String, usize>::new();
            let mut text_files = 0;
            let mut binary_files = 0;
            let mut records = 0;
            for a in &p.archives {
                for e in &a.entries {
                    if e.name.to_ascii_lowercase().ends_with(".txt") {
                        let script = shiinario_scenario::TextScript::parse(&e.name, &a.read(e)?)?;
                        text_files += 1;
                        records += script.records.len();
                        for (name, n) in script.inventory() {
                            *commands.entry(name).or_default() += n;
                        }
                    } else if e.name.to_ascii_lowercase().ends_with(".scn") {
                        binary_files += 1;
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"text_files":text_files,"text_records":records,"text_commands":commands,"binary_files":binary_files,"binary_instruction_coverage":"unresolved","completed_routes":0})
                )?
            );
        }
        Command::Trace {
            project_dir,
            name,
            max_steps,
            simulate_platform,
            tick_ms,
            input,
            best_effort,
            summary,
            final_frame,
            tail_events,
        } => {
            anyhow::ensure!(tail_events <= 10000, "tail-events exceeds 10000");
            let p = shiinario_runtime::open(project_dir)?;
            let input = input
                .map(|path| -> Result<_> { Ok(serde_json::from_slice(&std::fs::read(path)?)?) })
                .transpose()?
                .unwrap_or_default();
            let mut instructions = std::collections::BTreeMap::<u16, usize>::new();
            let mut skips = std::collections::BTreeMap::<u16, usize>::new();
            let mut glyphs = 0usize;
            let mut events = 0usize;
            let mut recent = std::collections::VecDeque::with_capacity(tail_events);
            let mut stories = Vec::new();
            let mut story_checkpoints = Vec::new();
            let mut clock_reply_pending = false;
            let mut last_clock_reply = 0u32;
            let mut elapsed_clock_replies = 0u64;
            let mut scenarios = std::collections::BTreeSet::new();
            scenarios.insert(name.clone());
            let result = shiinario_runtime::trace_with_options(
                &p,
                &name,
                max_steps,
                shiinario_runtime::TraceOptions {
                    simulate_platform,
                    tick_ms,
                    best_effort,
                    input,
                    final_frame: final_frame.map(
                        |path| -> Box<
                            dyn FnOnce(shiinario_runtime::resources::Surface) -> Result<()>,
                        > {
                            Box::new(move |frame| {
                                let file = std::fs::OpenOptions::new()
                                    .write(true)
                                    .create_new(true)
                                    .open(path)?;
                                let mut encoder =
                                    png::Encoder::new(file, frame.width, frame.height);
                                encoder.set_color(png::ColorType::Rgba);
                                encoder.set_depth(png::BitDepth::Eight);
                                encoder.write_header()?.write_image_data(&frame.rgba)?;
                                Ok(())
                            })
                        },
                    ),
                },
                |event| {
                    use shiinario_scenario::{Event, PlatformRequest};
                    events += 1;
                    if let Event::PlatformReply { value, .. } = event
                        && clock_reply_pending
                    {
                        elapsed_clock_replies += u64::from(value.wrapping_sub(last_clock_reply));
                        last_clock_reply = *value;
                    }
                    clock_reply_pending = matches!(
                        event,
                        Event::Platform {
                            request: PlatformRequest::ClockMilliseconds,
                            ..
                        }
                    );
                    if tail_events != 0 {
                        if recent.len() == tail_events {
                            recent.pop_front();
                        }
                        recent.push_back(event.clone());
                    }
                    match event {
                        Event::BinaryInstruction { opcode, .. } => {
                            *instructions.entry(*opcode).or_default() += 1
                        }
                        Event::CompatibilitySkip {
                            location,
                            opcode,
                            detail,
                        } => {
                            *skips.entry(*opcode).or_default() += 1;
                            eprintln!(
                                "SKIP {}:{:#x} opcode={opcode:#06x}: {detail}",
                                location.scenario, location.offset
                            );
                        }
                        Event::Platform {
                            request:
                                PlatformRequest::DrawGlyph { .. }
                                | PlatformRequest::DrawImageGlyph { .. },
                            ..
                        } => glyphs += 1,
                        Event::Platform {
                            request: PlatformRequest::LoadScenario { name, .. },
                            ..
                        } => {
                            scenarios.insert(name.clone());
                        }
                        Event::Platform {
                            request:
                                PlatformRequest::LoadAsset { name }
                                | PlatformRequest::ReadAssetInto { name, .. },
                            ..
                        } if name.to_ascii_lowercase().ends_with(".txt") => {
                            if summary {
                                eprintln!("STORY {name}");
                            }
                            stories.push(name.clone());
                            story_checkpoints.push(serde_json::json!({"name":name,"last_clock_reply_elapsed_ms":elapsed_clock_replies,"event":events}));
                        }
                        _ => {}
                    }
                    if !summary {
                        println!("{}", serde_json::to_string(event)?);
                    }
                    Ok(())
                },
            );
            if summary {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"events":events,"glyphs":glyphs,"loaded_scenarios":scenarios,"story_reads":stories,"story_checkpoints":story_checkpoints,"binary_instruction_counts":instructions,"skipped_instruction_counts":skips,"ended":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}")),"recent_events":recent})
                    )?
                );
            }
            result?;
        }
        Command::List { archive } => println!(
            "{}",
            serde_json::to_string_pretty(&Archive::open(archive)?.entries)?
        ),
        Command::Extract {
            archive,
            name,
            output,
        } => {
            let a = Archive::open(archive)?;
            let e = a.find(&name).context("entry not found")?;
            let d = a.read(e)?;
            std::fs::write(output, d)?;
        }
        Command::Inspect { project_dir } => {
            let p = shiinario_assets::project::Project::open(project_dir)?;
            println!("{}", serde_json::to_string_pretty(&p.config)?);
        }
        Command::Image {
            archive,
            name,
            frame,
            output,
        } => {
            let a = Archive::open(archive)?;
            let d = a.read(a.find(&name).context("entry not found")?)?;
            let f = shiinario_assets::image::decode(&d, frame)?;
            let mut e =
                png::Encoder::new(std::fs::File::create(output)?, f.info.width, f.info.height);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header()?.write_image_data(&f.rgba)?;
            println!("{}", serde_json::to_string(&f.info)?);
        }
        Command::Audio {
            archive,
            name,
            output,
            pcm,
        } => {
            let a = Archive::open(archive)?;
            let d = a.read(a.find(&name).context("entry not found")?)?;
            if pcm {
                use std::io::Write;
                let mut file = std::io::BufWriter::new(std::fs::File::create(output)?);
                let info = shiinario_assets::audio::decode(&d, |samples| {
                    for sample in samples {
                        file.write_all(&sample.to_le_bytes())?;
                    }
                    Ok(())
                })?;
                file.flush()?;
                println!("{}", serde_json::to_string(&info)?);
            } else {
                std::fs::write(output, shiinario_assets::audio::ogg_stream(&d)?)?;
            }
        }
        Command::Verify { project_dir, media } => {
            let mut paths = std::fs::read_dir(project_dir)?
                .map(|p| p.map(|p| p.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.sort();
            for path in paths {
                if !path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("war"))
                {
                    continue;
                }
                let a = Archive::open(&path)?;
                let mut total = 0;
                let mut hash = Sha256::new();
                let mut frames = 0usize;
                let mut pixels = Sha256::new();
                for e in &a.entries {
                    let d = a.read(e)?;
                    if media {
                        if e.name.to_ascii_lowercase().ends_with(".s25") {
                            for f in shiinario_assets::image::frames(&d)? {
                                let frame = shiinario_assets::image::decode(&d, f.index)
                                    .with_context(|| format!("{} frame {}", e.name, f.index))?;
                                frames += 1;
                                pixels.update((f.index as u64).to_le_bytes());
                                pixels.update(frame.rgba);
                            }
                        } else if e.name.to_ascii_lowercase().ends_with(".ogv") {
                            shiinario_assets::audio::decode(&d, |_| Ok(()))
                                .with_context(|| e.name.clone())?;
                        }
                    }
                    total += d.len();
                    hash.update(e.name.as_bytes());
                    hash.update((d.len() as u64).to_le_bytes());
                    hash.update(d);
                }
                if media {
                    eprintln!(
                        "{}\t{frames} frames\t{:x}",
                        path.file_name().unwrap().to_string_lossy(),
                        pixels.finalize()
                    );
                }
                println!(
                    "{}\t{} entries\t{} decoded bytes\t{:x}",
                    path.file_name().unwrap().to_string_lossy(),
                    a.entries.len(),
                    total,
                    hash.finalize()
                );
            }
        }
    }
    Ok(())
}
