//! `editor [MAP] [--shot FILE] [--tab 1-4]`: the Reformers map editor (#36, v2 #44). Edits a whole map:
//! the heights (MAP), the colours / surface materials (COL), the checkpoint layer (CHECK.PCX,
//! format in `track.rs`) and the texture tile sheet (MAPTEX.PCX, #45). It saves into the map's folder in each file's own format, keeping the
//! first original as `*.bak`; a running game on the same map picks the files up within a second
//! (hot reload), and F5 starts one.
//!
//! Layout: layer tabs and file buttons on top, the layer's tools on the left, the tool's settings
//! on the right, the cursor's cell at the bottom. Edits are tested in `edit.rs`.

mod edit;
mod ui;

use edit::{Brush, Edge, Layer, Shape};
use macroquad::prelude::*;
use reformers::terrain::SIZE;
use reformers::track::{BOOST, RESET, SLOW, SOFT, Track};
use reformers::{assets, pcx};
use std::path::{Path, PathBuf};
use ui::{Ui, text};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Height,
    Colour,
    Check,
    /// MAPTEX.PCX, the tile sheet.
    Tex,
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Brush,
    Pencil,
    Smooth,
    Ramp,
    Paint,
    Dot,
    /// The height / colour under the cursor (alt-click does it with any tool).
    Pick,
    Spawn,
    Gate,
    Renumber,
    Zone(u8),
    Erase,
    Fill,
}

fn tools(tab: Tab) -> &'static [(Tool, &'static str)] {
    match tab {
        Tab::Height => &[
            (Tool::Brush, "brush"),
            (Tool::Pencil, "pencil"),
            (Tool::Smooth, "smooth"),
            (Tool::Ramp, "ramp"),
            (Tool::Pick, "pick height"),
        ],
        Tab::Colour => &[
            (Tool::Paint, "brush"),
            (Tool::Dot, "pencil"),
            (Tool::Pick, "pick colour"),
        ],
        Tab::Check => &[
            (Tool::Spawn, "spawn"),
            (Tool::Gate, "gate"),
            (Tool::Renumber, "renumber"),
            (Tool::Zone(RESET), "reset zone"),
            (Tool::Zone(BOOST), "boost pad"),
            (Tool::Zone(SLOW), "slow zone"),
            (Tool::Zone(SOFT), "soft dirt"),
            (Tool::Erase, "eraser"),
        ],
        Tab::Tex => &[
            (Tool::Dot, "pencil"),
            (Tool::Paint, "brush"),
            (Tool::Fill, "fill"),
            (Tool::Pick, "pick colour"),
        ],
    }
}

/// One of the map's images as loaded, kept whole (legend strip, palette) to write back.
struct Source {
    /// MAP.PCX, MAP.PNG, COL.BMP, ...
    file: String,
    image: pcx::Image,
}

impl Source {
    fn load(map: &str, stem: &str) -> Result<Source, String> {
        let (path, bytes) = assets::read_map_image(map, stem)?;
        let image = pcx::decode_image(&bytes).map_err(|e| format!("{path}: {e}"))?;
        if image.width < SIZE || image.height < SIZE {
            return Err(format!("{path} is smaller than {SIZE}x{SIZE}"));
        }
        let file = path.rsplit('/').next().unwrap().to_string();
        Ok(Source { file, image })
    }

    /// `cells` in place of the image's top-left 256×256, written in the file's own format, or
    /// as PCX for TEREP1's BMPs (no BMP writer; the game reads the PCX first).
    fn write(&self, dir: &Path, cells: &[u8]) -> Result<String, String> {
        let mut img = self.image.clone();
        for (y, row) in cells.chunks_exact(SIZE).enumerate() {
            img.pixels[y * img.width..][..SIZE].copy_from_slice(row);
        }
        let (stem, ext) = self.file.rsplit_once('.').unwrap_or((&self.file, ""));
        let ext = ext.to_ascii_uppercase();
        let file = if ext == "BMP" {
            format!("{stem}.PCX")
        } else {
            self.file.clone()
        };
        let path = dir.join(&file);
        backup(&path)?;
        let fail = |e: std::io::Error| format!("{}: {e}", path.display());
        match ext.as_str() {
            "PGM" => {
                let head = format!("P5\n{} {}\n255\n", img.width, img.height);
                std::fs::write(&path, [head.into_bytes(), img.pixels].concat()).map_err(fail)?
            }
            "PNG" => {
                // export_png flips rows (it's made for GL screen grabs), so hand it flipped ones
                let bytes = img
                    .pixels
                    .chunks_exact(img.width)
                    .rev()
                    .flatten()
                    .flat_map(|&v| [v, v, v, 255])
                    .collect();
                macroquad::texture::Image {
                    bytes,
                    width: img.width as u16,
                    height: img.height as u16,
                }
                .export_png(&path.to_string_lossy())
            }
            _ => std::fs::write(&path, pcx::encode(&img)).map_err(fail)?,
        }
        Ok(file)
    }
}

/// Keeps the first original of `path` as `path.bak`.
fn backup(path: &Path) -> Result<(), String> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    if path.exists() && !Path::new(&bak).exists() {
        std::fs::copy(path, &bak).map_err(|e| format!("backing up {}: {e}", path.display()))?;
    }
    Ok(())
}

/// The map being edited.
struct Doc {
    name: String,
    dir: PathBuf,
    /// Heights 0–255 (×16 world units); fractional while editing so soft brushes build up.
    height: Vec<f32>,
    colour: Vec<u8>,
    check: Layer,
    palette: [[u8; 3]; 256],
    map: Source,
    col: Source,
    /// The MAPTEX tile sheet as the game picks it: the map folder's own, else the shared one when
    /// the palettes match, else flat tiles (what the game draws then). Saving writes the map's own.
    maptex: Vec<u8>,
    tex: Source,
    /// Where `maptex` came from: the map's own file, the shared one, or neither (flat).
    tex_from: &'static str,
    saved: (Vec<u8>, Vec<u8>, Layer, Vec<u8>),
}

impl Doc {
    fn load(name: &str) -> Result<Doc, String> {
        let dir = assets::map_dir(name).ok_or(format!("can't find map {name:?}"))?;
        let (map, col) = (Source::load(name, "MAP")?, Source::load(name, "COL")?);
        let check_path = dir.join("CHECK.PCX");
        let check = Layer(match std::fs::read(&check_path) {
            Ok(b) => pcx::decode(&b)
                .map(|i| i.crop(SIZE, SIZE).pixels)
                .map_err(|e| format!("{}: {e}", check_path.display()))?,
            Err(_) => vec![0; SIZE * SIZE],
        });
        let heights = map.image.crop(SIZE, SIZE).pixels;
        let colour = col.image.crop(SIZE, SIZE).pixels;
        let palette = col.image.palette;
        let sheet = |b: Vec<u8>| {
            pcx::decode(&b)
                .ok()
                .filter(|i| i.width >= SIZE && i.height >= SIZE)
        };
        let own = std::fs::read(dir.join("MAPTEX.PCX")).ok().and_then(sheet);
        let shared = || {
            assets::read("MAPTEX.PCX")
                .ok()
                .and_then(sheet)
                .filter(|i| i.palette == palette)
        };
        let (image, tex_from) = match own {
            Some(i) => (i, "this map's own"),
            None => match shared() {
                Some(i) => (i, "the shared"),
                None => (
                    pcx::Image {
                        width: SIZE,
                        height: SIZE,
                        pixels: edit::flat_sheet(),
                        palette,
                    },
                    "no",
                ),
            },
        };
        let maptex = image.crop(SIZE, SIZE).pixels;
        let tex = Source {
            file: "MAPTEX.PCX".into(),
            image,
        };
        Ok(Doc {
            name: name.into(),
            dir,
            height: heights.iter().map(|&v| v as f32).collect(),
            saved: (heights, colour.clone(), check.clone(), maptex.clone()),
            colour,
            check,
            palette,
            map,
            col,
            maptex,
            tex,
            tex_from,
        })
    }

    fn heights(&self) -> Vec<u8> {
        self.height
            .iter()
            .map(|&v| v.round().clamp(0.0, 255.0) as u8)
            .collect()
    }

    fn changed(&self) -> [bool; 4] {
        [
            self.heights() != self.saved.0,
            self.colour != self.saved.1,
            self.check != self.saved.2,
            self.maptex != self.saved.3,
        ]
    }

    /// Writes the changed layers (all three map layers with `all`); returns what was written.
    /// The tile sheet only when edited: it goes into this map's folder, never over the shared one.
    fn save(&mut self, all: bool) -> Result<String, String> {
        let [h, c, k, t] = self.changed();
        let [h, c, k] = [h, c, k].map(|c| c || all);
        let mut wrote = vec![];
        if h {
            wrote.push(self.map.write(&self.dir, &self.heights())?);
        }
        if c {
            wrote.push(self.col.write(&self.dir, &self.colour)?);
        }
        if k {
            let path = self.dir.join("CHECK.PCX");
            backup(&path)?;
            std::fs::write(&path, self.check.encode())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            wrote.push("CHECK.PCX".into());
        }
        if t {
            wrote.push(self.tex.write(&self.dir, &self.maptex)?);
            self.tex_from = "this map's own";
        }
        self.saved = (
            self.heights(),
            self.colour.clone(),
            self.check.clone(),
            self.maptex.clone(),
        );
        Ok(if wrote.is_empty() {
            "nothing to save".into()
        } else {
            format!("saved {} in {}", wrote.join(", "), self.dir.display())
        })
    }
}

/// A layer's contents before an edit, for undo / redo.
enum Snap {
    Height(Vec<f32>),
    Colour(Vec<u8>),
    Check(Layer),
    Tex(Vec<u8>),
}

impl Doc {
    fn snap(&self, tab: Tab) -> Snap {
        match tab {
            Tab::Height => Snap::Height(self.height.clone()),
            Tab::Colour => Snap::Colour(self.colour.clone()),
            Tab::Check => Snap::Check(self.check.clone()),
            Tab::Tex => Snap::Tex(self.maptex.clone()),
        }
    }

    /// Puts `s` back; returns what it replaced (for the other stack).
    fn restore(&mut self, s: Snap) -> Snap {
        match s {
            Snap::Height(v) => Snap::Height(std::mem::replace(&mut self.height, v)),
            Snap::Colour(v) => Snap::Colour(std::mem::replace(&mut self.colour, v)),
            Snap::Check(v) => Snap::Check(std::mem::replace(&mut self.check, v)),
            Snap::Tex(v) => Snap::Tex(std::mem::replace(&mut self.maptex, v)),
        }
    }
}

/// A new map folder next to `doc`'s: flat at `height`, all `colour`, in `doc`'s palette (and
/// its MAPTEX), no checkpoints.
fn create_map(doc: &Doc, name: &str, height: u8, colour: u8) -> Result<(), String> {
    let dir = new_dir(doc, name)?;
    let image = |v: u8, palette| pcx::Image {
        width: SIZE,
        height: SIZE,
        pixels: vec![v; SIZE * SIZE],
        palette,
    };
    let grey = std::array::from_fn(|i| [i as u8; 3]);
    let write = |f: &str, b: Vec<u8>| std::fs::write(dir.join(f), b).map_err(|e| e.to_string());
    write("MAP.PCX", pcx::encode(&image(height, grey)))?;
    write("COL.PCX", pcx::encode(&image(colour, doc.palette)))?;
    if let Ok(b) = std::fs::read(doc.dir.join("MAPTEX.PCX")) {
        write("MAPTEX.PCX", b)?;
    }
    Ok(())
}

/// A copy of `doc`'s folder (without backups and ghosts) under `name`.
fn copy_map(doc: &Doc, name: &str) -> Result<PathBuf, String> {
    let dir = new_dir(doc, name)?;
    for e in std::fs::read_dir(&doc.dir).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let f = e.file_name().to_string_lossy().to_string();
        if e.path().is_file() && !f.ends_with(".bak") && !f.starts_with("ghost-") {
            std::fs::copy(e.path(), dir.join(&f)).map_err(|e| format!("{f}: {e}"))?;
        }
    }
    Ok(dir)
}

fn new_dir(doc: &Doc, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    {
        return Err("a map name is letters, digits, _ and -".into());
    }
    let dir = doc.dir.parent().unwrap().join(name);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// The game, on this map, as its own process.
fn test_drive(map: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let game = exe.with_file_name(format!("reformers{}", std::env::consts::EXE_SUFFIX));
    std::process::Command::new(&game)
        .args(["--map", map])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't start {}: {e}", game.display()))
}

#[derive(Clone, Copy, PartialEq)]
enum Ask {
    New,
    SaveAs,
    /// The typed text filters the map list.
    Open,
}

/// The ground as seen from above: the colours shaded by slope (relief), or the heights as grey.
fn ground_rgba(doc: &Doc, relief: bool) -> Vec<u8> {
    let h = |x: usize, y: usize| doc.height[y.min(SIZE - 1) * SIZE + x.min(SIZE - 1)];
    (0..SIZE * SIZE)
        .flat_map(|i| {
            let (x, y) = (i % SIZE, i / SIZE);
            if !relief {
                let v = doc.height[i].round().clamp(0.0, 255.0) as u8;
                return [v, v, v, 255];
            }
            // lit from the top left: ground rising toward the bottom right faces the sun
            let slope = (h(x + 1, y) - h(x.saturating_sub(1), y)) * 0.4
                + (h(x, y + 1) - h(x, y.saturating_sub(1))) * 0.3;
            let shade = (0.85 + slope / 40.0).clamp(0.45, 1.15);
            let [r, g, b] =
                doc.palette[doc.colour[i] as usize].map(|c| (c as f32 * shade).min(255.0) as u8);
            [r, g, b, 255]
        })
        .collect()
}

fn texture(rgba: &[u8]) -> Texture2D {
    let t = Texture2D::from_rgba8(SIZE as u16, SIZE as u16, rgba);
    t.set_filter(FilterMode::Nearest);
    t
}

fn update(t: &Texture2D, rgba: Vec<u8>) {
    t.update(&Image {
        bytes: rgba,
        width: SIZE as u16,
        height: SIZE as u16,
    });
}

/// The piece (index into `pieces`) with a cell within 3 of (x, y): 1-cell gates are hard to hit.
fn piece_near(pieces: &[(u8, Vec<(usize, usize)>)], x: i32, y: i32) -> Option<usize> {
    pieces
        .iter()
        .enumerate()
        .flat_map(|(k, (_, cells))| cells.iter().map(move |&c| (k, c)))
        .map(|(k, (cx, cy))| (k, (cx as i32 - x).abs().max((cy as i32 - y).abs())))
        .filter(|&(_, d)| d <= 3)
        .min_by_key(|&(_, d)| d)
        .map(|(k, _)| k)
}

/// The maps whose names contain `filter` (any case).
fn matching<'a>(maps: &'a [String], filter: &str) -> Vec<&'a str> {
    let f = filter.to_lowercase();
    maps.iter()
        .filter(|m| m.to_lowercase().contains(&f))
        .map(String::as_str)
        .collect()
}

fn mean(cells: &[(usize, usize)]) -> Vec2 {
    let n = cells.len() as f32;
    cells
        .iter()
        .fold(Vec2::ZERO, |s, &(x, y)| s + vec2(x as f32, y as f32) / n)
}

/// The settings panel, top to bottom.
struct Column {
    x: f32,
    w: f32,
    y: f32,
}

impl Column {
    fn row(&mut self, h: f32) -> Rect {
        let r = Rect::new(self.x, self.y, self.w, h);
        self.y += h + 6.0;
        r
    }
}

fn window_conf() -> Conf {
    Conf {
        window_title: "Reformers map editor".into(),
        window_width: 1400,
        window_height: 900,
        high_dpi: true,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let maps = assets::maps();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // --shot FILE: save a screenshot after a moment and quit (for checking the look)
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let name = args
        .iter()
        .enumerate()
        .find(|&(i, a)| !a.starts_with("--") && (i == 0 || !args[i - 1].starts_with("--")))
        .map(|(_, a)| a)
        .cloned()
        .or_else(|| maps.first().cloned())
        .unwrap_or_else(|| fail("no maps found in assets/maps"));
    let mut doc =
        Doc::load(&name).unwrap_or_else(|e| fail(&format!("{e} (maps: {})", maps.join(", "))));
    prevent_quit();

    let mut ui = Ui::new();
    // --tab 1–3 opens on that layer (for --shot)
    let mut tab = match args
        .iter()
        .position(|a| a == "--tab")
        .and_then(|i| args.get(i + 1))
    {
        Some(n) if n == "2" => Tab::Colour,
        Some(n) if n == "3" => Tab::Check,
        Some(n) if n == "4" => Tab::Tex,
        _ => Tab::Height,
    };
    let mut tool = [Tool::Brush, Tool::Paint, Tool::Gate, Tool::Dot]; // per tab
    // settings
    let mut target = 64i32;
    let mut hbrush = Brush {
        shape: Shape::Circle,
        size: 9,
        edge: Edge::Smooth,
        feather: 4.0,
    };
    let (mut raise, mut rate, mut strength, mut passes, mut ramp_width) =
        (false, 30i32, 50i32, 2i32, 5i32);
    let mut cbrush = Brush {
        shape: Shape::Circle,
        size: 5,
        edge: Edge::Hard,
        feather: 0.0,
    };
    let mut colour = doc.colour[SIZE * SIZE / 2 + SIZE / 2];
    let (mut zone_size, mut zone_strength, mut facing) = (5i32, 8i32, 0u8);
    let mut relief = true;
    // the tile sheet: the tile being edited, the colour painted on it, the brush, noise
    let (mut tile, mut tsize, mut noise, mut seed) = (colour, 3i32, 2i32, 1u32);
    let mut tcolour = colour;
    let mut clip: Option<[u8; 256]> = None;
    let mut tlast: Option<[f32; 2]> = None; // a stroke on the tile, in its pixels
    // view
    let (mut zoom, mut pan) = (1.0f32, vec2(0.0, 0.0));
    let mut pan_from: Option<(Vec2, Vec2)> = None;
    // edits in progress
    let (mut undo, mut redo): (Vec<Snap>, Vec<Snap>) = (vec![], vec![]);
    let mut last: Option<[f32; 2]> = None; // where the stroke was last frame
    let mut set_base: Option<(Vec<f32>, Vec<f32>)> = None; // a "set" stroke's start and reach
    let mut drag: Option<(i32, i32)> = None; // ramp / gate line start
    let mut selected: Option<u8> = None; // gate setting
    let mut renumber: Vec<(usize, u8)> = vec![]; // clicked pieces, rank
    let mut ask: Option<(Ask, String)> = None;
    let mut map_list: Vec<String> = vec![];
    let mut open: Option<String> = None; // a map to switch to at the end of the frame
    let mut note = (String::new(), 0.0);
    let mut quit_asked = false;
    // derived, redone when the doc changes
    let ground = texture(&ground_rgba(&doc, relief));
    let overlay = texture(&[0; SIZE * SIZE * 4]);
    let swatches = texture(&[0; SIZE * SIZE * 4]);
    let (mut ground_dirty, mut check_dirty, mut swatch_dirty) = (true, true, true);
    let mut pieces: Vec<(u8, Vec<(usize, usize)>)> = vec![];
    let mut check = String::new();

    loop {
        ui.begin();
        let (w, h) = (screen_width(), screen_height());
        let dt = get_frame_time().min(0.1);
        let (top, left, right, bottom) = (40.0, 140.0, 280.0, 26.0);
        let area = Rect::new(left, top, w - left - right, h - top - bottom);
        let scale = area.w.min(area.h) / SIZE as f32 * zoom;
        let origin = area.center() - vec2(SIZE as f32, SIZE as f32) * scale / 2.0 + pan;
        let mouse = ui.mouse;
        let m = ((mouse - origin) / scale).to_array(); // cell position, fractional
        let (cx, cy) = (m[0].floor() as i32, m[1].floor() as i32);
        let on_map = (0..SIZE as i32).contains(&cx) && (0..SIZE as i32).contains(&cy);
        let cell =
            cy.clamp(0, SIZE as i32 - 1) as usize * SIZE + cx.clamp(0, SIZE as i32 - 1) as usize;
        let shift = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
        let alt = is_key_down(KeyCode::LeftAlt) || is_key_down(KeyCode::RightAlt);
        let cmd = is_key_down(KeyCode::LeftSuper)
            || is_key_down(KeyCode::RightSuper)
            || is_key_down(KeyCode::LeftControl)
            || is_key_down(KeyCode::RightControl);
        let t = tool[tab as usize];
        let mut action: Option<&str> = None; // a button or key asking for a file action

        // quitting with unsaved changes takes a second ask
        if is_quit_requested() {
            if doc.changed().contains(&true) && !quit_asked {
                quit_asked = true;
                note = (
                    "unsaved changes: save (Ctrl+S), or close again to drop them".into(),
                    get_time() + 6.0,
                );
            } else {
                break;
            }
        }

        // keys (typing a name takes them all)
        if let Some((kind, text)) = &mut ask {
            while let Some(c) = get_char_pressed() {
                if c.is_ascii_alphanumeric() || "_-".contains(c) {
                    text.push(c);
                }
            }
            if is_key_pressed(KeyCode::Backspace) {
                text.pop();
            }
            if is_key_pressed(KeyCode::Escape) {
                ask = None;
            } else if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                let (kind, text) = (*kind, text.clone());
                ask = None;
                let made = match kind {
                    Ask::Open => {
                        open = matching(&map_list, &text).first().map(|m| m.to_string());
                        Ok("")
                    }
                    Ask::New => create_map(&doc, &text, target as u8, colour).map(|_| "created"),
                    Ask::SaveAs => copy_map(&doc, &text).and_then(|dir| {
                        let (old_dir, old_name) = (doc.dir.clone(), doc.name.clone());
                        (doc.dir, doc.name) = (dir, text.clone());
                        doc.save(true).map(|_| "saved as").inspect_err(|_| {
                            (doc.dir, doc.name) = (old_dir, old_name);
                        })
                    }),
                };
                match made {
                    Ok("") => {}
                    Ok(how) => {
                        note = (format!("{how} {text}"), get_time() + 4.0);
                        open = Some(text);
                    }
                    Err(e) => note = (e, get_time() + 6.0),
                }
            }
        } else {
            while get_char_pressed().is_some() {}
            for (k, tb) in [
                (KeyCode::F1, Tab::Height),
                (KeyCode::F2, Tab::Colour),
                (KeyCode::F3, Tab::Check),
                (KeyCode::F4, Tab::Tex),
            ] {
                if is_key_pressed(k) {
                    tab = tb;
                }
            }
            if is_key_pressed(KeyCode::Tab) {
                tab = [Tab::Colour, Tab::Check, Tab::Tex, Tab::Height][tab as usize];
            }
            let keys = [
                KeyCode::Key1,
                KeyCode::Key2,
                KeyCode::Key3,
                KeyCode::Key4,
                KeyCode::Key5,
                KeyCode::Key6,
                KeyCode::Key7,
                KeyCode::Key8,
            ];
            for (k, &(tl, _)) in keys.iter().zip(tools(tab)) {
                if is_key_pressed(*k) {
                    tool[tab as usize] = tl;
                }
            }
            if tab == Tab::Check {
                for (k, d) in [
                    (KeyCode::Up, 0),
                    (KeyCode::Right, 1),
                    (KeyCode::Down, 2),
                    (KeyCode::Left, 3),
                ] {
                    if is_key_pressed(k) {
                        facing = d;
                        if let Some((x, y, _)) = doc.check.spawn() {
                            undo.push(doc.snap(Tab::Check));
                            doc.check.set_spawn(x.round() as i32, y.round() as i32, d);
                            check_dirty = true;
                        }
                    }
                }
                if let Some(s) = selected
                    && (is_key_pressed(KeyCode::Delete) || is_key_pressed(KeyCode::Backspace))
                {
                    undo.push(doc.snap(Tab::Check));
                    doc.check.erase_gate(s);
                    selected = None;
                    check_dirty = true;
                }
            }
            // [ ] resize what the tool paints with
            let grow = is_key_pressed(KeyCode::RightBracket) as i32
                - is_key_pressed(KeyCode::LeftBracket) as i32;
            if grow != 0 {
                match t {
                    Tool::Paint if tab == Tab::Tex => tsize = (tsize + grow).clamp(1, 8),
                    Tool::Brush | Tool::Smooth => {
                        hbrush.size = (hbrush.size as i32 + grow).clamp(1, 64) as u32
                    }
                    Tool::Paint => cbrush.size = (cbrush.size as i32 + grow).clamp(1, 64) as u32,
                    Tool::Ramp => ramp_width = (ramp_width + grow).clamp(1, 32),
                    Tool::Zone(_) | Tool::Erase => zone_size = (zone_size + grow).clamp(1, 32),
                    _ => {}
                }
            }
            if is_key_pressed(KeyCode::Z) || (cmd && is_key_pressed(KeyCode::Y)) {
                action = Some(if shift || is_key_pressed(KeyCode::Y) {
                    "redo"
                } else {
                    "undo"
                });
            }
            if is_key_pressed(KeyCode::S) {
                action = Some("save");
            }
            if is_key_pressed(KeyCode::O) {
                action = Some("open");
            }
            if is_key_pressed(KeyCode::F5) {
                action = Some("drive");
            }
            if is_key_pressed(KeyCode::Key0) {
                (zoom, pan) = (1.0, vec2(0.0, 0.0));
            }
            if is_key_pressed(KeyCode::Escape) {
                (drag, renumber) = (None, vec![]);
            }
        }

        // zoom at the cursor; pan with the right or middle button, or Space + left
        let over_map = area.contains(mouse) && ask.is_none() && !ui.busy() && tab != Tab::Tex;
        let (_, wheel) = mouse_wheel();
        if wheel != 0.0 && over_map {
            let before = (mouse - origin) / scale;
            zoom = (zoom * if wheel > 0.0 { 1.15 } else { 1.0 / 1.15 }).clamp(0.5, 24.0);
            let scale2 = area.w.min(area.h) / SIZE as f32 * zoom;
            pan = mouse
                - (area.center() - vec2(SIZE as f32, SIZE as f32) * scale2 / 2.0)
                - before * scale2;
        }
        let panning = is_mouse_button_down(MouseButton::Right)
            || is_mouse_button_down(MouseButton::Middle)
            || (is_key_down(KeyCode::Space) && is_mouse_button_down(MouseButton::Left));
        match (&pan_from, panning) {
            (None, true) if over_map => pan_from = Some((mouse, pan)),
            (Some((m0, p0)), true) => pan = *p0 + mouse - *m0,
            (_, false) => pan_from = None,
            _ => {}
        }

        // the map under the mouse
        let pressed = is_mouse_button_pressed(MouseButton::Left) && over_map && pan_from.is_none();
        let down = is_mouse_button_down(MouseButton::Left) && pan_from.is_none();
        let released = is_mouse_button_released(MouseButton::Left);
        if pressed && alt || pressed && t == Tool::Pick {
            if on_map {
                match tab {
                    Tab::Colour => (colour, swatch_dirty) = (doc.colour[cell], true),
                    _ => target = doc.height[cell].round() as i32,
                }
            }
        } else if pressed && on_map {
            if t == Tool::Pencil || (t == Tool::Brush && !raise) {
                set_base = Some((doc.height.clone(), vec![0.0; SIZE * SIZE]));
            }
            undo.push(doc.snap(tab));
            redo.clear();
            last = Some(m);
            match t {
                Tool::Ramp | Tool::Gate if on_map => drag = Some((cx, cy)),
                Tool::Spawn if on_map => {
                    doc.check.set_spawn(cx, cy, facing);
                    check_dirty = true;
                }
                Tool::Renumber => {
                    undo.pop();
                    if let Some(k) = piece_near(&pieces, cx, cy)
                        && !renumber.iter().any(|r| r.0 == k)
                    {
                        let n = renumber.iter().map(|r| r.1 + 1).max().unwrap_or(0);
                        let rank = if shift { n.saturating_sub(1) } else { n };
                        renumber.push((k, rank));
                    }
                }
                _ => {}
            }
        }
        // strokes: every frame the button is held, along the way the mouse went
        if down && let Some(from) = last {
            let path = edit::stroke(from, m);
            last = Some(m);
            let stamps = |b: &Brush| {
                path.iter()
                    .map(|&p| b.cells(b.centre(p)))
                    .collect::<Vec<_>>()
            };
            match t {
                Tool::Brush | Tool::Pencil if !alt => {
                    let b = if t == Tool::Pencil {
                        Brush::PENCIL
                    } else {
                        hbrush
                    };
                    if let Some((base, reach)) = &mut set_base {
                        for cells in stamps(&b) {
                            edit::set(&mut doc.height, base, reach, &cells, target as f32);
                        }
                    } else {
                        let sign = if shift { -1.0 } else { 1.0 };
                        edit::raise(
                            &mut doc.height,
                            &b.cells(b.centre(m)),
                            sign * rate as f32 * dt,
                        );
                    }
                    ground_dirty = true;
                }
                Tool::Smooth => {
                    let k = strength as f32 / 100.0 * dt * 10.0;
                    edit::smooth(&mut doc.height, &hbrush.cells(hbrush.centre(m)), k);
                    ground_dirty = true;
                }
                Tool::Paint | Tool::Dot if !alt => {
                    let b = if t == Tool::Dot {
                        Brush::PENCIL
                    } else {
                        cbrush
                    };
                    for cells in stamps(&b) {
                        edit::paint(&mut doc.colour, &cells, colour);
                    }
                    ground_dirty = true;
                }
                Tool::Zone(_) | Tool::Erase => {
                    let b = Brush {
                        shape: Shape::Circle,
                        size: zone_size as u32,
                        edge: Edge::Hard,
                        feather: 0.0,
                    };
                    let v = match t {
                        Tool::Zone(z) if z == BOOST || z == SLOW => z << 4 | zone_strength as u8,
                        Tool::Zone(z) => z << 4,
                        _ => 0,
                    };
                    for cells in stamps(&b) {
                        edit::paint(&mut doc.check.0, &cells, v);
                    }
                    check_dirty = true;
                }
                _ => {}
            }
        }
        if released {
            last = None;
            set_base = None;
            if let Some(a) = drag.take() {
                let b = (cx.clamp(0, 255), cy.clamp(0, 255));
                let au = (a.0 as usize, a.1 as usize);
                match t {
                    Tool::Ramp => {
                        edit::ramp(
                            &mut doc.height,
                            au,
                            (b.0 as usize, b.1 as usize),
                            ramp_width as u32,
                            target as f32,
                        );
                        ground_dirty = true;
                    }
                    // a click (no drag) on a gate picks it; a drag draws one
                    Tool::Gate if a == b => {
                        undo.pop();
                        selected = piece_near(&pieces, a.0, a.1).map(|k| pieces[k].0);
                    }
                    Tool::Gate => {
                        let n = match selected {
                            Some(s) => doc.check.insert_after(s),
                            None => doc.check.next_gate(),
                        };
                        match n {
                            Some(n) => {
                                doc.check.gate_line(a, b, n);
                                selected = Some(n);
                                check_dirty = true;
                            }
                            None => {
                                note = (
                                    "16 gates is the most CHECK.PCX holds".into(),
                                    get_time() + 4.0,
                                )
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // renumbering ends when every piece has its number, or on Enter
        if t != Tool::Renumber {
            renumber.clear();
        } else if !renumber.is_empty()
            && (renumber.len() == pieces.len() || is_key_pressed(KeyCode::Enter))
        {
            undo.push(doc.snap(Tab::Check));
            match doc.check.renumber(&renumber) {
                Ok(()) => note = ("gates renumbered".into(), get_time() + 3.0),
                Err(e) => {
                    undo.pop();
                    note = (e, get_time() + 5.0);
                }
            }
            renumber.clear();
            check_dirty = true;
        }

        // derived views
        if ground_dirty {
            ground_dirty = false;
            update(&ground, ground_rgba(&doc, relief));
        }
        if check_dirty {
            check_dirty = false;
            let rgba = doc
                .check
                .0
                .iter()
                .flat_map(|&v| edit::colour(v).map(|x| (x * 255.0) as u8))
                .collect();
            update(&overlay, rgba);
            pieces = doc.check.pieces();
            if selected.is_some_and(|s| !doc.check.gates().contains(&s)) {
                selected = None;
            }
            check = match Track::load(&doc.check.encode()) {
                Ok(t) => format!(
                    "{} gates{}{}",
                    t.gates.len(),
                    if t.spawn.is_some() {
                        ", spawn set"
                    } else {
                        ", no spawn (cars start mid-map)"
                    },
                    if t.gates.len() == 1 {
                        " (a lap needs 2 or more)"
                    } else {
                        ""
                    }
                ),
                Err(e) => format!("won't load: {e}"),
            };
        }
        if swatch_dirty {
            swatch_dirty = false;
            // MAPTEX tiles (row = material, column = shade), else the palette's colours
            let rgba = (0..SIZE * SIZE)
                .flat_map(|i| {
                    let [r, g, b] = doc.palette[doc.maptex[i] as usize];
                    [r, g, b, 255]
                })
                .collect();
            update(&swatches, rgba);
        }

        // the tile sheet tab: the whole sheet (pick a tile) beside the tile, big (paint on it)
        clear_background(Color::from_rgba(24, 26, 30, 255));
        let side = (area.h - 20.0).min(area.w / 2.0 - 20.0);
        let sheet_r = Rect::new(area.x + 10.0, area.y + 10.0, side, side);
        let tile_r = Rect::new(sheet_r.right() + 20.0, area.y + 10.0, side, side);
        if tab == Tab::Tex {
            let free = ask.is_none() && !ui.busy();
            let press = is_mouse_button_pressed(MouseButton::Left) && free;
            if press && sheet_r.contains(mouse) {
                let q = ((mouse - sheet_r.point()) / side * 16.0)
                    .as_ivec2()
                    .clamp(IVec2::ZERO, IVec2::splat(15));
                tile = (q.y * 16 + q.x) as u8;
            }
            let p = ((mouse - tile_r.point()) / side * 16.0).to_array();
            let (px, py) = (p[0].floor() as i32, p[1].floor() as i32);
            let on_tile = tile_r.contains(mouse);
            if press && on_tile {
                if alt || t == Tool::Pick {
                    tcolour = doc.maptex[edit::tile_px(tile, px as usize, py as usize)];
                } else {
                    undo.push(doc.snap(Tab::Tex));
                    redo.clear();
                    if t == Tool::Fill {
                        edit::tile_fill(&mut doc.maptex, tile, px as usize, py as usize, tcolour);
                        swatch_dirty = true;
                    } else {
                        tlast = Some([p[0] - 0.01, p[1]]);
                    }
                }
            }
            if !is_mouse_button_down(MouseButton::Left) {
                tlast = None;
            }
            if let Some(from) = tlast {
                // a stroke wraps at the tile's edges, so draw it in the tile's own pixels
                let size = if t == Tool::Paint { tsize } else { 1 };
                for q in edit::stroke(from, p) {
                    let (x, y) = (q[0].floor() as i32, q[1].floor() as i32);
                    edit::tile_dab(&mut doc.maptex, tile, x, y, size, tcolour);
                }
                tlast = Some(p);
                swatch_dirty = true;
            }
            for (r, src) in [
                (sheet_r, None),
                (
                    tile_r,
                    Some(Rect::new(
                        (tile & 15) as f32 * 16.0,
                        (tile >> 4) as f32 * 16.0,
                        16.0,
                        16.0,
                    )),
                ),
            ] {
                draw_texture_ex(
                    &swatches,
                    r.x,
                    r.y,
                    WHITE,
                    DrawTextureParams {
                        dest_size: Some(vec2(r.w, r.h)),
                        source: src,
                        ..Default::default()
                    },
                );
            }
            let cell = side / 16.0;
            draw_rectangle_lines(
                sheet_r.x + (tile & 15) as f32 * cell,
                sheet_r.y + (tile >> 4) as f32 * cell,
                cell,
                cell,
                2.0,
                YELLOW,
            );
            draw_rectangle_lines(tile_r.x, tile_r.y, tile_r.w, tile_r.h, 1.0, GRAY);
            if on_tile && free {
                let size = if t == Tool::Paint { tsize as f32 } else { 1.0 };
                let off = if size as i32 % 2 == 0 { 0.0 } else { 0.5 };
                let c = tile_r.point() + (vec2(px as f32, py as f32) + off) * cell;
                draw_rectangle_lines(
                    c.x - size / 2.0 * cell,
                    c.y - size / 2.0 * cell,
                    size * cell,
                    size * cell,
                    1.5,
                    WHITE,
                );
            }
        }

        // drawing: the map
        if tab != Tab::Tex {
            let size = vec2(SIZE as f32, SIZE as f32) * scale;
            let fade = if tab == Tab::Check { 1.0 } else { 0.35 };
            for (tex, tint) in [
                (&ground, WHITE),
                (&overlay, Color::new(1.0, 1.0, 1.0, fade)),
            ] {
                draw_texture_ex(
                    tex,
                    origin.x,
                    origin.y,
                    tint,
                    DrawTextureParams {
                        dest_size: Some(size),
                        ..Default::default()
                    },
                );
            }
            let at = |p: Vec2| origin + (p + vec2(0.5, 0.5)) * scale;
            if tab == Tab::Check {
                // gates: number labels (S = start/finish), arrows on to the next gate's pieces
                let order = doc.check.gates();
                let rank = |s: u8| order.iter().position(|&u| u == s).unwrap();
                for (s, cells) in pieces.iter().filter(|_| order.len() > 1) {
                    let next = order[(rank(*s) + 1) % order.len()];
                    for (_, to) in pieces.iter().filter(|p| p.0 == next) {
                        let (a, b) = (at(mean(cells)), at(mean(to)));
                        let d = (b - a).normalize_or_zero();
                        let mid = a + (b - a) * 0.55;
                        let c = Color::new(1.0, 1.0, 1.0, 0.6);
                        draw_line(a.x, a.y, b.x, b.y, 1.5, c);
                        let side = vec2(-d.y, d.x) * 6.0;
                        draw_triangle(mid + d * 10.0, mid + side, mid - side, c);
                    }
                }
                for (k, (s, cells)) in pieces.iter().enumerate() {
                    let p = at(mean(cells));
                    let r = rank(*s);
                    let (label, ink) = match renumber.iter().find(|q| q.0 == k) {
                        Some(q) if q.1 == 0 => ("S".to_string(), GREEN),
                        Some(q) => (q.1.to_string(), GREEN),
                        None if t == Tool::Renumber => ("?".into(), GRAY),
                        None if r == 0 => ("S".into(), YELLOW),
                        None => (r.to_string(), YELLOW),
                    };
                    if selected == Some(*s) {
                        draw_circle_lines(p.x, p.y, 15.0, 3.0, SKYBLUE);
                    }
                    draw_circle(p.x, p.y, 11.0, Color::new(0.0, 0.0, 0.0, 0.75));
                    let d = measure_text(&label, None, 20, 1.0);
                    draw_text(&label, p.x - d.width / 2.0, p.y + 6.0, 20.0, ink);
                }
                if let Some((x, y, d)) = doc.check.spawn() {
                    let p = at(vec2(x, y));
                    let v = [
                        vec2(0.0, -1.0),
                        vec2(1.0, 0.0),
                        vec2(0.0, 1.0),
                        vec2(-1.0, 0.0),
                    ][d as usize];
                    let tip = p + v * 22.0;
                    draw_line(p.x, p.y, tip.x, tip.y, 4.0, WHITE);
                    draw_circle(p.x, p.y, 6.0, WHITE);
                    draw_circle(tip.x, tip.y, 4.0, WHITE);
                }
            }
            // the tool under the mouse
            if over_map && on_map {
                let outline = |b: &Brush| {
                    let c = origin + Vec2::from(b.centre(m)) * scale;
                    let r = b.size as f32 / 2.0 * scale;
                    match b.shape {
                        Shape::Circle => draw_circle_lines(c.x, c.y, r, 1.5, WHITE),
                        Shape::Rect => {
                            draw_rectangle_lines(c.x - r, c.y - r, 2.0 * r, 2.0 * r, 1.5, WHITE)
                        }
                    }
                };
                match t {
                    Tool::Brush | Tool::Smooth => outline(&hbrush),
                    Tool::Paint => outline(&cbrush),
                    Tool::Pencil | Tool::Dot | Tool::Pick => outline(&Brush::PENCIL),
                    Tool::Zone(_) | Tool::Erase => outline(&Brush {
                        shape: Shape::Circle,
                        size: zone_size as u32,
                        edge: Edge::Hard,
                        feather: 0.0,
                    }),
                    _ => {}
                }
            }
            if let Some(a) = drag {
                let (p, q) = (
                    at(vec2(a.0 as f32, a.1 as f32)),
                    at(vec2(cx as f32, cy as f32)),
                );
                if t == Tool::Ramp {
                    let side = vec2(-(q - p).y, (q - p).x).normalize_or_zero() * ramp_width as f32
                        / 2.0
                        * scale;
                    for (u, v) in [
                        (p + side, q + side),
                        (q + side, q - side),
                        (q - side, p - side),
                        (p - side, p + side),
                    ] {
                        draw_line(u.x, u.y, v.x, v.y, 2.0, WHITE);
                    }
                    let from = doc.height[a.1 as usize * SIZE + a.0 as usize].round();
                    text(&format!("{from} to {target}"), q.x + 12.0, q.y, WHITE);
                } else {
                    draw_line(p.x, p.y, q.x, q.y, 3.0, YELLOW);
                }
            }
        }

        // panels
        ui::panel(Rect::new(0.0, 0.0, w, top));
        ui::panel(Rect::new(0.0, top, left, h - top - bottom));
        ui::panel(Rect::new(w - right, top, right, h - top - bottom));
        ui::panel(Rect::new(0.0, h - bottom, w, bottom));
        // top: layers, view, file
        let mut x = 8.0;
        for (tb, label) in [
            (Tab::Height, "Heights F1"),
            (Tab::Colour, "Colours F2"),
            (Tab::Check, "Checkpoints F3"),
            (Tab::Tex, "Textures F4"),
        ] {
            if ui.button(Rect::new(x, 6.0, 120.0, 28.0), label, tab == tb) {
                tab = tb;
            }
            x += 124.0;
        }
        x += 16.0;
        let was = relief;
        ui.choice(
            Rect::new(x, 6.0, 170.0, 28.0),
            &[(true, "relief"), (false, "heights")],
            &mut relief,
        );
        ground_dirty |= relief != was;
        x += 186.0;
        let unsaved = doc.changed().contains(&true);
        text(
            &format!("{}{}", doc.name, if unsaved { " *" } else { "" }),
            x,
            26.0,
            WHITE,
        );
        let mut bx = w - 8.0;
        for (label, act, bw) in [
            ("Test drive F5", "drive", 120.0),
            ("New", "new", 56.0),
            ("Open", "open", 60.0),
            ("Save as", "save as", 76.0),
            ("Save", "save", 60.0),
            ("Redo", "redo", 60.0),
            ("Undo", "undo", 60.0),
        ] {
            bx -= bw + 4.0;
            if ui.button(Rect::new(bx, 6.0, bw, 28.0), label, false) {
                action = Some(act);
            }
        }
        // left: the layer's tools
        for (i, &(tl, label)) in tools(tab).iter().enumerate() {
            let r = Rect::new(8.0, top + 8.0 + i as f32 * 34.0, left - 16.0, 30.0);
            if ui.button(r, &format!("{} {label}", i + 1), t == tl) {
                tool[tab as usize] = tl;
            }
        }
        // right: the tool's settings
        let (px, pw) = (w - right + 10.0, right - 20.0);
        let mut col = Column {
            x: px,
            w: pw,
            y: top + 10.0,
        };
        let name = tools(tab).iter().find(|x| x.0 == t).map_or("", |x| x.1);
        text(name, px, col.row(20.0).y + 15.0, WHITE);
        let mut size_i;
        if tab == Tab::Tex {
            let line = format!("tile {tile}: material {}, shade {}", tile >> 4, tile & 15);
            text(&line, px, col.row(18.0).y + 13.0, WHITE);
            let from = format!("from {} sheet; saves as this map's", doc.tex_from);
            text(&from, px, col.row(18.0).y + 13.0, GRAY);
            if t == Tool::Paint {
                ui.slider(col.row(26.0), "size", &mut tsize, 1, 8);
            }
            // the palette: what to paint with
            let g = col.row(pw);
            let c = g.w / 16.0;
            for v in 0..=255u8 {
                let [r, gg, b] = doc.palette[v as usize];
                let (x, y) = (g.x + (v & 15) as f32 * c, g.y + (v >> 4) as f32 * c);
                draw_rectangle(x, y, c, c, Color::from_rgba(r, gg, b, 255));
                if v == tcolour {
                    draw_rectangle_lines(x, y, c, c, 2.0, YELLOW);
                }
            }
            if g.contains(mouse) && is_mouse_button_pressed(MouseButton::Left) {
                let q = ((mouse - g.point()) / c)
                    .as_ivec2()
                    .clamp(IVec2::ZERO, IVec2::splat(15));
                tcolour = (q.y * 16 + q.x) as u8;
            }
            text(
                &format!("painting colour {tcolour}"),
                px,
                col.row(18.0).y + 13.0,
                WHITE,
            );
            col.row(4.0);
            ui.slider(col.row(26.0), "noise shades", &mut noise, 0, 7);
            if ui.button(col.row(28.0), "Noise from this colour", false) {
                undo.push(doc.snap(Tab::Tex));
                redo.clear();
                seed = seed.wrapping_add(1);
                edit::tile_noise(&mut doc.maptex, tile, tcolour, noise as u8, seed);
                swatch_dirty = true;
            }
            let r = col.row(28.0);
            let half = (r.w - 4.0) / 2.0;
            if ui.button(Rect::new(r.x, r.y, half, r.h), "Copy tile", false) {
                clip = Some(edit::tile_copy(&doc.maptex, tile));
            }
            if ui.button(
                Rect::new(r.x + half + 4.0, r.y, half, r.h),
                "Paste tile",
                false,
            ) && let Some(c) = &clip
            {
                undo.push(doc.snap(Tab::Tex));
                redo.clear();
                edit::tile_paste(&mut doc.maptex, tile, c);
                swatch_dirty = true;
            }
            // as the game lays it: once per cell, side by side
            text("tiled", px, col.row(18.0).y + 13.0, GRAY);
            let g = col.row(pw / 2.0);
            let n = 4;
            let c = g.h / n as f32;
            for i in 0..n * 2 {
                for j in 0..n {
                    draw_texture_ex(
                        &swatches,
                        g.x + i as f32 * c,
                        g.y + j as f32 * c,
                        WHITE,
                        DrawTextureParams {
                            dest_size: Some(vec2(c, c)),
                            source: Some(Rect::new(
                                (tile & 15) as f32 * 16.0,
                                (tile >> 4) as f32 * 16.0,
                                16.0,
                                16.0,
                            )),
                            ..Default::default()
                        },
                    );
                }
            }
        } else {
            match t {
                Tool::Brush | Tool::Smooth => {
                    ui.choice(
                        col.row(26.0),
                        &[(Shape::Circle, "circle"), (Shape::Rect, "square")],
                        &mut hbrush.shape,
                    );
                    size_i = hbrush.size as i32;
                    ui.slider(col.row(26.0), "size", &mut size_i, 1, 64);
                    hbrush.size = size_i as u32;
                    ui.choice(
                        col.row(26.0),
                        &[
                            (Edge::Hard, "hard"),
                            (Edge::Smooth, "smooth"),
                            (Edge::Feather, "feather"),
                        ],
                        &mut hbrush.edge,
                    );
                    if hbrush.edge == Edge::Feather {
                        let mut f = hbrush.feather as i32;
                        ui.slider(col.row(26.0), "feather", &mut f, 1, 32);
                        hbrush.feather = f as f32;
                    }
                    if t == Tool::Brush {
                        ui.choice(
                            col.row(26.0),
                            &[(false, "set height"), (true, "raise / lower")],
                            &mut raise,
                        );
                        if raise {
                            ui.slider(col.row(26.0), "rate /s", &mut rate, 1, 128);
                            text("hold Shift to lower", px, col.row(18.0).y + 13.0, GRAY);
                        } else {
                            ui.slider(col.row(26.0), "target height", &mut target, 0, 255);
                        }
                    } else {
                        ui.slider(col.row(26.0), "strength", &mut strength, 1, 100);
                        col.row(6.0);
                        text("whole map", px, col.row(18.0).y + 13.0, GRAY);
                        ui.slider(col.row(26.0), "passes", &mut passes, 1, 20);
                        if ui.button(col.row(28.0), "Smooth whole map", false) {
                            undo.push(doc.snap(Tab::Height));
                            redo.clear();
                            edit::smooth_all(&mut doc.height, passes as u32);
                            ground_dirty = true;
                        }
                    }
                }
                Tool::Pencil | Tool::Pick if tab == Tab::Height => {
                    ui.slider(col.row(26.0), "target height", &mut target, 0, 255);
                }
                Tool::Ramp => {
                    ui.slider(col.row(26.0), "width", &mut ramp_width, 1, 32);
                    ui.slider(col.row(26.0), "end height", &mut target, 0, 255);
                    text(
                        "drag from the start; it starts at the",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                    text(
                        "height there and ends at this one",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                }
                Tool::Paint | Tool::Dot | Tool::Pick => {
                    if t == Tool::Paint {
                        ui.choice(
                            col.row(26.0),
                            &[(Shape::Circle, "circle"), (Shape::Rect, "square")],
                            &mut cbrush.shape,
                        );
                        size_i = cbrush.size as i32;
                        ui.slider(col.row(26.0), "size", &mut size_i, 1, 64);
                        cbrush.size = size_i as u32;
                    }
                    // the swatches: 16 materials (rows) × 16 shades (columns)
                    let g = col.row(pw);
                    draw_texture_ex(
                        &swatches,
                        g.x,
                        g.y,
                        WHITE,
                        DrawTextureParams {
                            dest_size: Some(vec2(g.w, g.h)),
                            ..Default::default()
                        },
                    );
                    let tile = g.w / 16.0;
                    let mark = |v: u8, c: Color| {
                        let (r, k) = ((v >> 4) as f32, (v & 15) as f32);
                        draw_rectangle_lines(g.x + k * tile, g.y + r * tile, tile, tile, 2.0, c);
                    };
                    let mut shown = colour;
                    if g.contains(mouse) {
                        let v = (((mouse.y - g.y) / tile) as u8).min(15) << 4
                            | (((mouse.x - g.x) / tile) as u8).min(15);
                        mark(v, WHITE);
                        shown = v;
                        if is_mouse_button_pressed(MouseButton::Left) {
                            colour = v;
                            if t == Tool::Pick {
                                tool[tab as usize] = Tool::Paint;
                            }
                        }
                    }
                    mark(colour, YELLOW);
                    let [r, gg, b] = doc.palette[shown as usize];
                    draw_rectangle(px, col.y, 26.0, 26.0, Color::from_rgba(r, gg, b, 255));
                    text(
                        &format!(
                            "colour {shown}: material {}, shade {}",
                            shown >> 4,
                            shown & 15
                        ),
                        px + 32.0,
                        col.y + 18.0,
                        WHITE,
                    );
                    col.row(26.0);
                    if doc.tex_from == "no" {
                        text(
                            "(no MAPTEX for this palette: flat colours)",
                            px,
                            col.row(18.0).y + 13.0,
                            GRAY,
                        );
                    }
                }
                Tool::Spawn => {
                    ui.choice(
                        col.row(26.0),
                        &[(0, "up"), (1, "right"), (2, "down"), (3, "left")],
                        &mut facing,
                    );
                    text(
                        "click to place; arrows turn it",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                }
                Tool::Zone(_) | Tool::Erase => {
                    ui.slider(col.row(26.0), "size", &mut zone_size, 1, 32);
                    if matches!(t, Tool::Zone(z) if z == BOOST || z == SLOW) {
                        ui.slider(col.row(26.0), "strength", &mut zone_strength, 0, 15);
                    }
                }
                Tool::Gate => {
                    text(
                        "drag a line across the road; it goes",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                    text(
                        "after the selected gate (click one)",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                }
                Tool::Renumber => {
                    text(
                        "click the gates in driving order;",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                    text(
                        "Shift-click: same number (a fork)",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                    text(
                        "Enter applies, Esc starts over",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
                }
                _ => {}
            }
        }
        if tab == Tab::Check {
            // the gates in lap order: select, move, delete
            col.row(8.0);
            text("gates (lap order)", px, col.row(18.0).y + 13.0, GRAY);
            let order = doc.check.gates();
            for (rank, &s) in order.iter().enumerate() {
                let r = col.row(24.0);
                let n = pieces.iter().filter(|p| p.0 == s).count();
                let label = format!(
                    "{}{}",
                    if rank == 0 {
                        "S start".to_string()
                    } else {
                        format!("{rank}")
                    },
                    if n > 1 {
                        format!("  fork x{n}")
                    } else {
                        String::new()
                    }
                );
                if ui.button(
                    Rect::new(r.x, r.y, r.w - 96.0, r.h),
                    &label,
                    selected == Some(s),
                ) {
                    selected = if selected == Some(s) { None } else { Some(s) };
                }
                let edits = [("up", Some(true)), ("dn", Some(false)), ("x", None)];
                for (i, (l, up)) in edits.into_iter().enumerate() {
                    if ui.button(
                        Rect::new(r.x + r.w - 92.0 + i as f32 * 31.0, r.y, 29.0, r.h),
                        l,
                        false,
                    ) {
                        undo.push(doc.snap(Tab::Check));
                        redo.clear();
                        match up {
                            Some(up) => doc.check.move_gate(rank, up),
                            None => doc.check.erase_gate(s),
                        }
                        check_dirty = true;
                    }
                }
            }
        }

        // file actions
        match action {
            Some("undo") | Some("redo") => {
                let (from, to) = if action == Some("undo") {
                    (&mut undo, &mut redo)
                } else {
                    (&mut redo, &mut undo)
                };
                if let Some(s) = from.pop() {
                    to.push(doc.restore(s));
                    (ground_dirty, check_dirty, swatch_dirty) = (true, true, true);
                }
            }
            Some("save") => {
                note = match doc.save(false) {
                    Ok(m) => (m, get_time() + 4.0),
                    Err(e) => (e, get_time() + 8.0),
                };
                quit_asked = false;
            }
            Some("drive") => {
                note = match doc.save(false).and_then(|_| test_drive(&doc.name)) {
                    Ok(()) => (
                        format!("saved; starting the game on {}", doc.name),
                        get_time() + 4.0,
                    ),
                    Err(e) => (e, get_time() + 8.0),
                };
            }
            // (a new or opened map replaces this one in the window)
            Some("new" | "open") if unsaved => {
                note = (
                    "save first: the map you open replaces this one".into(),
                    get_time() + 5.0,
                )
            }
            Some("new") => ask = Some((Ask::New, String::new())),
            Some("open") => {
                map_list = assets::maps();
                ask = Some((Ask::Open, String::new()));
            }
            Some("save as") => ask = Some((Ask::SaveAs, format!("{}-2", doc.name))),
            _ => {}
        }

        // bottom: the cell under the mouse
        if on_map && tab != Tab::Tex {
            let v = doc.colour[cell];
            text(
                &format!(
                    "({cx}, {cy})   height {:.0}   colour {v} (material {}, shade {})   zoom {:.0}%",
                    doc.height[cell],
                    v >> 4,
                    v & 15,
                    zoom * 100.0
                ),
                10.0,
                h - 8.0,
                WHITE,
            );
        }
        let help = "wheel zoom | right-drag pan | 0 fit | [ ] size | Alt-click pick | Z undo | Shift+Z redo | S save | O open";
        let status = if tab == Tab::Check {
            check.as_str()
        } else {
            help
        };
        let d = measure_text(status, None, ui::TEXT as u16, 1.0);
        text(status, w - d.width - 10.0, h - 8.0, GRAY);
        if get_time() < note.1 {
            let d = measure_text(&note.0, None, 22, 1.0);
            let (nx, ny) = ((w - d.width) / 2.0, h - bottom - 14.0);
            draw_rectangle(
                nx - 8.0,
                ny - 20.0,
                d.width + 16.0,
                28.0,
                Color::new(0.0, 0.0, 0.0, 0.8),
            );
            draw_text(&note.0, nx, ny, 22.0, YELLOW);
        }
        if let Some((Ask::Open, filter)) = &ask {
            // the map folders in columns, narrowed by what's typed; click one
            let list = matching(&map_list, filter);
            let (cols, cw, rh) = (5, 150.0, 22.0);
            let rows = ((h - 220.0) / rh).max(1.0) as usize;
            let r = Rect::new(w / 2.0 - 390.0, 70.0, 780.0, 110.0 + rows as f32 * rh);
            draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.05, 0.05, 0.06, 0.97));
            draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, YELLOW);
            text(
                "Open map: type to filter, click one or Enter for the first",
                r.x + 16.0,
                r.y + 28.0,
                WHITE,
            );
            draw_text(format!("{filter}_"), r.x + 16.0, r.y + 60.0, 26.0, YELLOW);
            for (i, m) in list.iter().take(cols * rows).enumerate() {
                let b = Rect::new(
                    r.x + 16.0 + (i / rows) as f32 * cw,
                    r.y + 76.0 + (i % rows) as f32 * rh,
                    cw - 4.0,
                    rh - 2.0,
                );
                if ui.button(b, m, *m == doc.name) {
                    open = Some(m.to_string());
                }
            }
            let more = list.len().saturating_sub(cols * rows);
            let foot = if more > 0 {
                format!("{more} more: type to narrow   Esc: cancel")
            } else {
                "Esc: cancel".into()
            };
            text(&foot, r.x + 16.0, r.y + r.h - 10.0, GRAY);
        } else if let Some((kind, name)) = &ask {
            let r = Rect::new(w / 2.0 - 260.0, h / 2.0 - 60.0, 520.0, 120.0);
            draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.05, 0.05, 0.06, 0.97));
            draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, YELLOW);
            let q = match kind {
                Ask::New => format!("New flat map, height {target}, colour {colour}. Folder name:"),
                Ask::SaveAs => "Save a copy of this map as folder:".into(),
                Ask::Open => unreachable!(),
            };
            text(&q, r.x + 16.0, r.y + 30.0, WHITE);
            draw_text(format!("{name}_"), r.x + 16.0, r.y + 68.0, 26.0, YELLOW);
            text("Enter: ok   Esc: cancel", r.x + 16.0, r.y + 100.0, GRAY);
        }

        // another map in the window: opened, created or saved as
        if let Some(name) = open.take() {
            ask = None;
            match Doc::load(&name) {
                Ok(d) => {
                    doc = d;
                    (undo, redo, selected) = (vec![], vec![], None);
                    (ground_dirty, check_dirty, swatch_dirty) = (true, true, true);
                }
                Err(e) => note = (e, get_time() + 6.0),
            }
        }

        if let Some(f) = &shot
            && get_time() > 1.0
        {
            get_screen_data().export_png(f);
            break;
        }
        next_frame().await;
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Heights written back come out of the loader the same, in every format the game reads.
    #[test]
    fn writes_back_in_each_format() {
        let dir = std::env::temp_dir().join(format!("reformers-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cells: Vec<u8> = (0..SIZE * SIZE).map(|i| (i * 7 % 251) as u8).collect();
        let blank = pcx::Image {
            width: 320,
            height: SIZE,
            pixels: vec![9; 320 * SIZE],
            palette: std::array::from_fn(|i| [i as u8; 3]),
        };
        for file in ["MAP.PCX", "MAP.PGM", "MAP.PNG", "MAP.BMP"] {
            let src = Source {
                file: file.into(),
                image: blank.clone(),
            };
            std::fs::write(dir.join(file), b"original").unwrap();
            let wrote = src.write(&dir, &cells).unwrap();
            let back = pcx::decode_image(&std::fs::read(dir.join(&wrote)).unwrap()).unwrap();
            assert_eq!(back.crop(SIZE, SIZE).pixels, cells, "{file}");
            if file == "MAP.BMP" {
                assert_eq!(wrote, "MAP.PCX", "BMPs come back as PCX");
            } else {
                assert_eq!(back.pixels[300], 9, "{file}: the legend strip stays");
                let bak = std::fs::read(dir.join(format!("{file}.bak"))).unwrap();
                assert_eq!(bak, b"original", "{file}: the original is kept");
            }
            // a second save keeps the first backup
            src.write(&dir, &cells).unwrap();
            if file != "MAP.BMP" {
                assert_eq!(
                    std::fs::read(dir.join(format!("{file}.bak"))).unwrap(),
                    b"original"
                );
            }
            for f in [
                file.to_string(),
                format!("{file}.bak"),
                "MAP.PCX".into(),
                "MAP.PCX.bak".into(),
            ] {
                let _ = std::fs::remove_file(dir.join(f));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
