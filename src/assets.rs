//! Finds the original game files (MAP.PCX, CAR1.DAT, ...).
//! Looked up in order: `$REFORMERS_ASSETS`, `./assets`, `assets/` next to the executable,
//! and the source checkout's `assets/` (so `cargo run` works from anywhere).

use std::path::PathBuf;

fn dirs() -> Vec<PathBuf> {
    let mut d: Vec<PathBuf> = std::env::var_os("REFORMERS_ASSETS")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    d.push("assets".into());
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("assets")))
    {
        d.push(exe_dir);
    }
    d.push(concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into());
    d
}

/// When an asset was last modified (None if missing), for hot reload. Same lookup as `read`.
pub fn modified(name: &str) -> Option<std::time::SystemTime> {
    dirs()
        .iter()
        .find_map(|d| std::fs::metadata(d.join(name)).ok())?
        .modified()
        .ok()
}

/// Reads an asset by file name, e.g. `read("MAP.PCX")`.
pub fn read(name: &str) -> Result<Vec<u8>, String> {
    let dirs = dirs();
    dirs.iter()
        .find_map(|d| std::fs::read(d.join(name)).ok())
        .ok_or_else(|| {
            format!(
                "{name} not found in {} (set REFORMERS_ASSETS to the game files)",
                dirs.iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}
