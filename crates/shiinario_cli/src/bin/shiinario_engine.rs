use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Run the game in the current directory (binary SCN support is incomplete)")]
struct Args {
    /// Stop at known presentation omissions instead of printing SKIP and continuing.
    #[arg(long)]
    strict: bool,
    /// Close after this many milliseconds (native startup smoke test).
    #[arg(long)]
    run_for_ms: Option<u64>,
    /// Write modified SCN buffers on exit to a new directory outside the game.
    #[arg(long)]
    scenario_dump: Option<PathBuf>,
}
fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    let project = shiinario_runtime::open(".")?;
    shiinario_runtime::native::run(
        &project,
        shiinario_runtime::native::Options {
            strict: args.strict,
            run_for: args.run_for_ms.map(std::time::Duration::from_millis),
            scenario_dump: args.scenario_dump,
        },
    )
}
