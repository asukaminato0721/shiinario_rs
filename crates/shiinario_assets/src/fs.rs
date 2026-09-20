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
