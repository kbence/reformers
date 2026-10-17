//! Finds the original game files (MAP.PCX, CAR1.DAT, ...).
//! Looked up in order: `$REFORMERS_ASSETS`, `./assets`, `assets/` next to the executable,
//! and the source checkout's `assets/` (so `cargo run` works from anywhere).
//! On the web there's no disk: `preload` fetches the files over HTTP once, `read` serves them.

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

#[cfg(target_arch = "wasm32")]
static WEB_FILES: std::sync::Mutex<Vec<(String, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());

/// Shared game files every map needs (preloaded on the web).
#[cfg(target_arch = "wasm32")]
const SHARED: [&str; 9] = [
    "MAPTEX.PCX",
    "TEXTURES.PCX",
    "CAR1.DAT",
    "CAR2.DAT",
    "CAR3.DAT",
    "CAR4.DAT",
    "CAR5.DAT",
    "CAR1.PCX",
    "CAR2.PCX",
];

/// Web only: fetches the shared files and every map's files (from `maps/index.txt`, as the
/// web can't list folders) into memory, so `read` works there. Does nothing natively.
pub async fn preload() {
    #[cfg(target_arch = "wasm32")]
    {
        let fetch = |name: String| async move {
            let bytes = macroquad::file::load_file(&format!("assets/{name}"))
                .await
                .ok();
            if let Some(b) = bytes {
                WEB_FILES.lock().unwrap().push((name, b));
            }
        };
        fetch("maps/index.txt".into()).await;
        for name in SHARED {
            fetch(name.into()).await;
        }
        for map in maps() {
            for f in [
                "MAP.PCX",
                "MAP.BMP",
                "COL.PCX",
                "COL.BMP",
                "CHECK.PCX",
                "MAPTEX.PCX",
            ] {
                fetch(format!("maps/{map}/{f}")).await;
            }
        }
    }
}

/// Map folders (`maps/<name>/` with MAP + COL) in the first asset dir that has any, sorted.
/// On the web: the names in `maps/index.txt`.
pub fn maps() -> Vec<String> {
    if cfg!(target_arch = "wasm32") {
        let index = read("maps/index.txt").unwrap_or_default();
        return String::from_utf8_lossy(&index)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
    }
    dirs()
        .iter()
        .find_map(|d| {
            let mut names: Vec<String> = std::fs::read_dir(d.join("maps"))
                .ok()?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            names.sort();
            (!names.is_empty()).then_some(names)
        })
        .unwrap_or_default()
}

/// A map's image: `maps/<map>/<stem>.PCX`, or `.BMP` (TEREP1). Returns the file name and bytes.
pub fn read_map_image(map: &str, stem: &str) -> Result<(String, Vec<u8>), String> {
    let pcx = format!("maps/{map}/{stem}.PCX");
    let bmp = format!("maps/{map}/{stem}.BMP");
    read(&pcx)
        .map(|b| (pcx, b))
        .or_else(|_| read(&bmp).map(|b| (bmp.clone(), b)))
        .map_err(|_| format!("map {map:?} has no {stem}.PCX or {stem}.BMP"))
}

/// When an asset was last modified (None if missing), for hot reload. Same lookup as `read`.
pub fn modified(name: &str) -> Option<std::time::SystemTime> {
    if cfg!(target_arch = "wasm32") {
        return None; // no files to watch (and no clock: SystemTime panics there)
    }
    dirs()
        .iter()
        .find_map(|d| std::fs::metadata(d.join(name)).ok())?
        .modified()
        .ok()
}

/// Reads an asset by file name, e.g. `read("MAP.PCX")`.
#[cfg(target_arch = "wasm32")]
pub fn read(name: &str) -> Result<Vec<u8>, String> {
    WEB_FILES
        .lock()
        .unwrap()
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.clone())
        .ok_or_else(|| format!("{name} not found in assets/"))
}

#[cfg(not(target_arch = "wasm32"))]
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
