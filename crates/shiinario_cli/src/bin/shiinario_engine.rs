use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Ran→Sem compatibility runtime (binary SCN support is incomplete)")]
struct Args {
    #[arg(long)]
    project_dir: PathBuf,
    /// Close after this many milliseconds (native startup smoke test).
    #[arg(long)]
    run_for_ms: Option<u64>,
}
fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    let project = shiinario_runtime::open(args.project_dir)?;
    shiinario_runtime::native::run(
        &project,
        shiinario_runtime::native::Options {
            run_for: args.run_for_ms.map(std::time::Duration::from_millis),
        },
    )
}
