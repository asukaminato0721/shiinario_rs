//! File access shared by native hosts and the browser worker.
#[cfg(not(target_arch = "wasm32"))]
pub use std::fs::*;
#[cfg(not(target_arch = "wasm32"))]
pub fn is_dir(path: &std::path::Path) -> bool {
    path.is_dir()
}
#[cfg(not(target_arch = "wasm32"))]
pub fn is_file(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(target_arch = "wasm32")]
#[path = "browser_fs.rs"]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;

/// Find EXEs below the game root without following directory symlinks.
/// File symlinks remain valid candidates, as in the original flat search.
pub(crate) fn executable_paths(
    directory: &std::path::Path,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    use anyhow::{Context, ensure};
    let mut directories = vec![(directory.to_owned(), 0)];
    let mut paths = Vec::new();
    while let Some((directory, depth)) = directories.pop() {
        ensure!(depth < 32, "executable directory nesting exceeds limit");
        for entry in read_dir(&directory)
            .with_context(|| format!("searching for game executables in {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() && !kind.is_symlink() {
                directories.push((path, depth + 1));
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                && is_file(&path)
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}
