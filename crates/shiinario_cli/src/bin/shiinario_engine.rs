use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Ran→Sem compatibility runtime (binary SCN execution is not yet supported)")]
struct Args {
    #[arg(long)]
    project_dir: PathBuf,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let project = shiinario_runtime::open(args.project_dir)?;
    shiinario_runtime::boot(&project)
}
