//! `editor [MAP] [--shot FILE] [--tab 1-6]`: the Reformers map editor (#36, v2 #44); without MAP,
//! the map it last had open. Edits a whole map:
//! the heights (MAP), the colours / surface materials (COL), the checkpoint layer (CHECK.PCX,
//! format in `track.rs`), the texture tile sheet (MAPTEX.PCX, #45), and the scenery objects
//! (OBJECTS.PCX) with their sprites (OBJTEX.PCX, #50). It saves into the map's folder in each file's own format, keeping the
//! first original as `*.bak`; a running game on the same map picks the files up within a second
//! (hot reload), and F5 starts one.
//!
//! Layout: layer tabs and file buttons on top, the layer's tools on the left, the tool's settings
//! on the right, the cursor's cell at the bottom. Edits are tested in `edit.rs`.

mod edit;
mod ui;

use edit::{Brush, Edge, Layer, Shape};
use macroquad::prelude::*;
use reformers::sim::{DAMP, DIG_COLOUR, GRIP, MAX_LOAD};
use reformers::terrain::SIZE;
use reformers::track::{BOOST, RESET, SLOW, SOFT, Track};
use reformers::{assets, objects, pcx};
use std::path::{Path, PathBuf};
use ui::{Ui, text};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Height,
    Colour,
    Check,
    /// MAPTEX.PCX, the tile sheet.
    Tex,
    /// OBJECTS.PCX: trees and the like on the map.
    Objects,
    /// OBJTEX.PCX: the object sprites.
    Sprites,
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
    /// Objects: the forest brush (random kinds and sizes at a density).
    Scatter,
    /// Objects: one at a time.
    Place,
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
        Tab::Objects => &[
            (Tool::Scatter, "forest brush"),
            (Tool::Place, "place one"),
            (Tool::Erase, "eraser"),
            (Tool::Pick, "pick"),
        ],
        Tab::Sprites => &[
            (Tool::Dot, "pencil"),
            (Tool::Paint, "brush"),
            (Tool::Fill, "fill"),
            (Tool::Erase, "eraser"),
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
    /// OBJECTS.PCX: one object per cell (`reformers::objects`), 0 = none.
    objects: Vec<u8>,
    /// OBJTEX.PCX: the object sprites and their own palette (colour 0 clear); the map's own
    /// sheet, else the shared one. Saving writes the map's own.
    objtex: Vec<u8>,
    objpal: [[u8; 3]; 256],
    obj: Source,
    obj_from: &'static str,
    /// Each kind's trunk (OBJTEX.TXT next to the sheet, else measured off the sprite).
    colliders: objects::Colliders,
    /// Each layer's bytes as last loaded or saved (see `layers`).
    saved: [Vec<u8>; 7],
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
        let objects = std::fs::read(dir.join("OBJECTS.PCX"))
            .ok()
            .and_then(sheet)
            .map_or(vec![0; SIZE * SIZE], |i| i.crop(SIZE, SIZE).pixels);
        // the colliders go with the sheet, as the game reads them: the map's own OBJTEX.TXT,
        // else the shared one unless the map has its own sheet
        let own_sheet = dir.join("OBJTEX.PCX").exists();
        let txt = std::fs::read(dir.join("OBJTEX.TXT"))
            .ok()
            .or_else(|| {
                (!own_sheet)
                    .then(|| assets::read("OBJTEX.TXT").ok())
                    .flatten()
            })
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let (image, obj_from) = match std::fs::read(dir.join("OBJTEX.PCX")).ok().and_then(sheet) {
            Some(i) => (i, "this map's own"),
            None => match assets::read("OBJTEX.PCX").ok().and_then(sheet) {
                Some(i) => (i, "the shared"),
                None => (
                    pcx::Image {
                        width: SIZE,
                        height: SIZE,
                        pixels: vec![0; SIZE * SIZE],
                        palette: std::array::from_fn(|i| [i as u8; 3]),
                    },
                    "no",
                ),
            },
        };
        let colliders = objects::colliders(txt.as_deref(), Some(&image))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut doc = Doc {
            name: name.into(),
            dir,
            height: heights.iter().map(|&v| v as f32).collect(),
            colour,
            check,
            palette,
            map,
            col,
            maptex,
            tex,
            tex_from,
            objects,
            objtex: image.crop(SIZE, SIZE).pixels,
            objpal: image.palette,
            obj: Source {
                file: "OBJTEX.PCX".into(),
                image,
            },
            obj_from,
            colliders,
            saved: Default::default(),
        };
        doc.saved = doc.layers();
        Ok(doc)
    }

    /// Every layer's bytes, to tell what changed: heights, colours, checkpoints, tiles, objects,
    /// sprites (with their palette).
    fn layers(&self) -> [Vec<u8>; 7] {
        [
            self.heights(),
            self.colour.clone(),
            self.check.0.clone(),
            self.maptex.clone(),
            self.objects.clone(),
            [self.objtex.clone(), self.objpal.concat()].concat(),
            self.colliders
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ]
    }

    fn heights(&self) -> Vec<u8> {
        self.height
            .iter()
            .map(|&v| v.round().clamp(0.0, 255.0) as u8)
            .collect()
    }

    fn changed(&self) -> [bool; 7] {
        let now = self.layers();
        std::array::from_fn(|i| now[i] != self.saved[i])
    }

    /// Writes the changed layers (all three map layers with `all`); returns what was written.
    /// The sheets only when edited: they go into this map's folder, never over the shared ones.
    fn save(&mut self, all: bool) -> Result<String, String> {
        let [h, c, k, t, o, sp, co] = self.changed();
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
        if o {
            let path = self.dir.join("OBJECTS.PCX");
            backup(&path)?;
            let img = pcx::Image {
                width: SIZE,
                height: SIZE,
                pixels: self.objects.clone(),
                palette: reformers::objects::palette(),
            };
            std::fs::write(&path, pcx::encode(&img))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            wrote.push("OBJECTS.PCX".into());
        }
        if sp {
            self.obj.image.palette = self.objpal;
            wrote.push(self.obj.write(&self.dir, &self.objtex)?);
            self.obj_from = "this map's own";
        }
        // with the map's own sheet the shared colliders no longer apply: write them too
        if co || sp {
            let path = self.dir.join("OBJTEX.TXT");
            backup(&path)?;
            std::fs::write(&path, objects::colliders_text(&self.colliders))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            wrote.push("OBJTEX.TXT".into());
        }
        self.saved = self.layers();
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
    Objects(Vec<u8>),
    Sprites(Vec<u8>, [[u8; 3]; 256], objects::Colliders),
}

impl Doc {
    fn snap(&self, tab: Tab) -> Snap {
        match tab {
            Tab::Height => Snap::Height(self.height.clone()),
            Tab::Colour => Snap::Colour(self.colour.clone()),
            Tab::Check => Snap::Check(self.check.clone()),
            Tab::Tex => Snap::Tex(self.maptex.clone()),
            Tab::Objects => Snap::Objects(self.objects.clone()),
            Tab::Sprites => Snap::Sprites(self.objtex.clone(), self.objpal, self.colliders),
        }
    }

    /// Puts `s` back; returns what it replaced (for the other stack).
    fn restore(&mut self, s: Snap) -> Snap {
        match s {
            Snap::Height(v) => Snap::Height(std::mem::replace(&mut self.height, v)),
            Snap::Colour(v) => Snap::Colour(std::mem::replace(&mut self.colour, v)),
            Snap::Check(v) => Snap::Check(std::mem::replace(&mut self.check, v)),
            Snap::Tex(v) => Snap::Tex(std::mem::replace(&mut self.maptex, v)),
            Snap::Objects(v) => Snap::Objects(std::mem::replace(&mut self.objects, v)),
            Snap::Sprites(v, p, c) => Snap::Sprites(
                std::mem::replace(&mut self.objtex, v),
                std::mem::replace(&mut self.objpal, p),
                std::mem::replace(&mut self.colliders, c),
            ),
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

/// How the map shows the ground: shaded by slope in its colours or its MAPTEX tiles, or the
/// heights as grey.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Colour,
    Texture,
    Heights,
}

/// The ground as seen from above (see `View`). For `Texture` it is the shader's input instead:
/// the COL index in red and the shade / 2 in green, the tiles come from `TEXTURED`.
fn ground_rgba(doc: &Doc, view: View) -> Vec<u8> {
    let h = |x: usize, y: usize| doc.height[y.min(SIZE - 1) * SIZE + x.min(SIZE - 1)];
    (0..SIZE * SIZE)
        .flat_map(|i| {
            let (x, y) = (i % SIZE, i / SIZE);
            if view == View::Heights {
                let v = doc.height[i].round().clamp(0.0, 255.0) as u8;
                return [v, v, v, 255];
            }
            // lit from the top left: ground rising toward the bottom right faces the sun
            let slope = (h(x + 1, y) - h(x.saturating_sub(1), y)) * 0.4
                + (h(x, y + 1) - h(x, y.saturating_sub(1))) * 0.3;
            let shade = (0.85 + slope / 40.0).clamp(0.45, 1.15);
            if view == View::Texture {
                return [doc.colour[i], (shade * 127.5) as u8, 0, 255];
            }
            let [r, g, b] =
                doc.palette[doc.colour[i] as usize].map(|c| (c as f32 * shade).min(255.0) as u8);
            [r, g, b, 255]
        })
        .collect()
}

/// A grey checkerboard of `n` squares across `r`: what shows through colour 0.
fn checker(r: Rect, n: usize) {
    let c = r.w / n as f32;
    let rows = (r.h / c).round() as usize;
    draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.32, 0.33, 0.36, 1.0));
    for y in 0..rows {
        for x in (y % 2..n).step_by(2) {
            let (px, py) = (r.x + x as f32 * c, r.y + y as f32 * c);
            draw_rectangle(px, py, c, c, Color::new(0.42, 0.43, 0.46, 1.0));
        }
    }
}

/// Textured relief: each cell's MAPTEX tile (from `MapTex`, the swatch sheet) times its shade,
/// fading to the tile's average colour (`Means`, 16×16) as cells shrink below a few pixels, where
/// the tiles would only shimmer. `Detail` is 0 (averages) to 1 (tiles).
const TEXTURED_VERTEX: &str = "#version 100
precision highp float;
attribute vec3 position;
attribute vec2 texcoord;
attribute vec4 color0;
varying vec2 uv;
varying vec4 tint;
uniform mat4 Model;
uniform mat4 Projection;
void main() {
    gl_Position = Projection * Model * vec4(position, 1);
    uv = texcoord;
    tint = color0 / 255.0;
}
";
// ponytail: highp so fract(uv * 256) holds up when zoomed in; the editor is desktop-only
const TEXTURED: &str = "#version 100
precision highp float;
varying vec2 uv;
varying vec4 tint;
uniform sampler2D Texture;
uniform sampler2D MapTex;
uniform sampler2D Means;
uniform float Detail;
void main() {
    vec2 cell = uv * 256.0;
    vec4 g = texture2D(Texture, (floor(cell) + 0.5) / 256.0);
    float idx = floor(g.r * 255.0 + 0.5);
    vec2 tile = vec2(mod(idx, 16.0), floor(idx / 16.0));
    vec3 near = texture2D(MapTex, (tile * 16.0 + 0.5 + fract(cell) * 15.0) / 256.0).rgb;
    vec3 far = texture2D(Means, (tile + 0.5) / 16.0).rgb;
    gl_FragColor = vec4(mix(far, near, Detail) * g.g * 2.0, 1.0) * tint;
}
";

/// Each MAPTEX tile's average colour, a 16×16 image (tile `c` at column `c & 15`, row `c >> 4`).
fn tile_means(doc: &Doc) -> Vec<u8> {
    (0..256)
        .flat_map(|t| {
            let (tx, ty) = (t % 16 * 16, t / 16 * 16);
            let mut sum = [0u32; 3];
            for y in ty..ty + 16 {
                for x in tx..tx + 16 {
                    let c = doc.palette[doc.maptex[y * SIZE + x] as usize];
                    (0..3).for_each(|k| sum[k] += c[k] as u32);
                }
            }
            let [r, g, b] = sum.map(|v| (v / 256) as u8);
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
        icon: Some(reformers::icon::icon()),
        ..Default::default()
    }
}

/// The map the editor last had open, so it opens on it again (`editor.txt` in the data folder).
fn last_map_file() -> Option<std::path::PathBuf> {
    Some(assets::data_dir()?.join("editor.txt"))
}

fn last_map() -> Option<String> {
    let s = std::fs::read_to_string(last_map_file()?).ok()?;
    Some(s.trim().to_string())
}

fn remember_map(name: &str) {
    let Some(path) = last_map_file() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Err(e) = std::fs::write(&path, name) {
        eprintln!("can't save {}: {e}", path.display());
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
        .or_else(|| last_map().filter(|m| maps.contains(m)))
        .or_else(|| maps.first().cloned())
        .unwrap_or_else(|| fail("no maps found in assets/maps"));
    let mut doc =
        Doc::load(&name).unwrap_or_else(|e| fail(&format!("{e} (maps: {})", maps.join(", "))));
    remember_map(&name);
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
        Some(n) if n == "5" => Tab::Objects,
        Some(n) if n == "6" => Tab::Sprites,
        _ => Tab::Height,
    };
    let mut tool = [
        Tool::Brush,
        Tool::Paint,
        Tool::Gate,
        Tool::Dot,
        Tool::Scatter,
        Tool::Dot,
    ]; // per tab
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
    let mut view = View::Colour;
    // the tile sheet: the tile being edited, the colour painted on it, the brush, noise
    let (mut tile, mut tsize, mut noise, mut seed) = (colour, 3i32, 2i32, 1u32);
    let mut tcolour = colour;
    let mut clip: Option<Vec<u8>> = None;
    let mut tlast: Option<[f32; 2]> = None; // a stroke on the tile, in its pixels
    // objects: the kinds the brush picks from, sizes, density, brush; a stroke's visited cells
    let mut kinds: Vec<u8> = vec![1, 2, 4];
    let (mut size_lo, mut size_hi, mut density, mut obj_size) = (3i32, 8i32, 30i32, 9i32);
    let mut seen = vec![false; SIZE * SIZE];
    let mut orng = 0x2545_F491u32;
    // sprites: the one being drawn, its colour and brush; a stroke on it, in its pixels
    let (mut skind, mut spcolour, mut spsize) = (1u8, 1u8, 2i32);
    let mut slast: Option<[f32; 2]> = None;
    let mut sclip: Option<Vec<u8>> = None;
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
    let ground = texture(&ground_rgba(&doc, view));
    let means = Texture2D::from_rgba8(16, 16, &tile_means(&doc));
    means.set_filter(FilterMode::Nearest);
    let textured = load_material(
        ShaderSource::Glsl {
            vertex: TEXTURED_VERTEX,
            fragment: TEXTURED,
        },
        MaterialParams {
            textures: vec!["MapTex".into(), "Means".into()],
            uniforms: vec![UniformDesc::new("Detail", UniformType::Float1)],
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| fail(&format!("the textured view's shader: {e}")));
    let overlay = texture(&[0; SIZE * SIZE * 4]);
    // the cells of the colour hovered in the picker (Shift: its whole material), blinking
    let lit = texture(&[0; SIZE * SIZE * 4]);
    let mut hovered: Option<u8> = None;
    let swatches = texture(&[0; SIZE * SIZE * 4]);
    let (mut ground_dirty, mut check_dirty, mut swatch_dirty) = (true, true, true);
    let objlayer = texture(&[0; SIZE * SIZE * 4]);
    let sprite_tex = texture(&[0; SIZE * SIZE * 4]);
    let (mut objects_dirty, mut sprites_dirty) = (true, true);
    let mut sprite_rects: [Option<([usize; 4], [f32; 2])>; 16] = [None; 16];
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
                (KeyCode::F6, Tab::Objects),
                (KeyCode::F7, Tab::Sprites),
            ] {
                if is_key_pressed(k) {
                    tab = tb;
                }
            }
            if is_key_pressed(KeyCode::Tab) {
                tab = [
                    Tab::Colour,
                    Tab::Check,
                    Tab::Tex,
                    Tab::Objects,
                    Tab::Sprites,
                    Tab::Height,
                ][tab as usize];
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
                    Tool::Paint | Tool::Erase if tab == Tab::Sprites => {
                        spsize = (spsize + grow).clamp(1, 8)
                    }
                    Tool::Scatter | Tool::Erase if tab == Tab::Objects => {
                        obj_size = (obj_size + grow).clamp(1, 32)
                    }
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
        let over_map = area.contains(mouse)
            && ask.is_none()
            && !ui.busy()
            && !matches!(tab, Tab::Tex | Tab::Sprites);
        let obrush = Brush {
            shape: Shape::Circle,
            size: obj_size as u32,
            edge: Edge::Hard,
            feather: 0.0,
        };
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
                    Tab::Objects => {
                        let v = doc.objects[cell];
                        if v >> 4 != 0 {
                            kinds = vec![v >> 4];
                            (size_lo, size_hi) = ((v & 15) as i32, (v & 15) as i32);
                        }
                    }
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
            seen.fill(false);
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
                Tool::Scatter => {
                    let (lo, hi) = (size_lo as u8, size_hi as u8);
                    let d = density as f32 / 100.0;
                    for cells in stamps(&obrush) {
                        edit::scatter(
                            &mut doc.objects,
                            &cells,
                            &mut seen,
                            &kinds,
                            (lo, hi),
                            d,
                            &mut orng,
                        );
                    }
                    objects_dirty = true;
                }
                Tool::Place if !alt => {
                    // replaces what's there, once per cell per stroke
                    let (lo, hi) = (size_lo as u8, size_hi as u8);
                    for cells in stamps(&Brush::PENCIL) {
                        for &(i, _) in &cells {
                            if !seen[i] {
                                doc.objects[i] = 0;
                            }
                        }
                        edit::scatter(
                            &mut doc.objects,
                            &cells,
                            &mut seen,
                            &kinds,
                            (lo, hi),
                            1.0,
                            &mut orng,
                        );
                    }
                    objects_dirty = true;
                }
                Tool::Erase if tab == Tab::Objects => {
                    for cells in stamps(&obrush) {
                        edit::paint(&mut doc.objects, &cells, 0);
                    }
                    objects_dirty = true;
                }
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
            update(&ground, ground_rgba(&doc, view));
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
            means.update(&Image {
                bytes: tile_means(&doc),
                width: 16,
                height: 16,
            });
        }
        if objects_dirty {
            objects_dirty = false;
            let pal = objects::palette();
            let rgba = doc
                .objects
                .iter()
                .flat_map(|&v| {
                    let [r, g, b] = pal[v as usize];
                    [r, g, b, if v >> 4 == 0 { 0 } else { 255 }]
                })
                .collect();
            update(&objlayer, rgba);
        }
        if sprites_dirty {
            sprites_dirty = false;
            let rgba = doc
                .objtex
                .iter()
                .flat_map(|&v| {
                    let [r, g, b] = doc.objpal[v as usize];
                    [r, g, b, if v == 0 { 0 } else { 255 }]
                })
                .collect();
            update(&sprite_tex, rgba);
            let img = pcx::Image {
                width: SIZE,
                height: SIZE,
                pixels: doc.objtex.clone(),
                palette: doc.objpal,
            };
            sprite_rects = std::array::from_fn(|k| objects::sprite(&img, k as u8));
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
                    tcolour = doc.maptex[edit::Cel::tile(tile).px(px as usize, py as usize)];
                } else {
                    undo.push(doc.snap(Tab::Tex));
                    redo.clear();
                    if t == Tool::Fill {
                        edit::Cel::tile(tile).fill(
                            &mut doc.maptex,
                            px as usize,
                            py as usize,
                            tcolour,
                        );
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
                    edit::Cel::tile(tile).dab(&mut doc.maptex, x, y, size, tcolour);
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

        // the sprites tab: the whole OBJTEX sheet (pick a sprite) beside the sprite, big
        let spr_r = Rect::new(sheet_r.right() + 20.0, area.y + 10.0, side / 2.0, side);
        if tab == Tab::Sprites {
            let free = ask.is_none() && !ui.busy();
            let press = is_mouse_button_pressed(MouseButton::Left) && free;
            if press && sheet_r.contains(mouse) {
                let q = ((mouse - sheet_r.point()) / side * vec2(8.0, 4.0))
                    .as_ivec2()
                    .clamp(IVec2::ZERO, ivec2(7, 3));
                let slot = (q.y * 8 + q.x) as u8;
                if slot < 15 {
                    skind = slot + 1;
                }
            }
            let cel = edit::Cel::sprite(skind);
            let px_size = side / 64.0;
            let p = ((mouse - spr_r.point()) / px_size).to_array();
            let (px, py) = (p[0].floor() as i32, p[1].floor() as i32);
            let on = spr_r.contains(mouse) && free;
            if press && on {
                if alt || t == Tool::Pick {
                    spcolour = doc.objtex[cel.px(px as usize, py as usize)];
                } else {
                    undo.push(doc.snap(Tab::Sprites));
                    redo.clear();
                    if t == Tool::Fill {
                        cel.fill(&mut doc.objtex, px as usize, py as usize, spcolour);
                        sprites_dirty = true;
                    } else {
                        slast = Some([p[0] - 0.01, p[1]]);
                    }
                }
            }
            if !is_mouse_button_down(MouseButton::Left) {
                slast = None;
            }
            if let Some(from) = slast {
                let (size, v) = match t {
                    Tool::Paint => (spsize, spcolour),
                    Tool::Erase => (spsize, 0),
                    _ => (1, spcolour),
                };
                for q in edit::stroke(from, p) {
                    cel.dab(
                        &mut doc.objtex,
                        q[0].floor() as i32,
                        q[1].floor() as i32,
                        size,
                        v,
                    );
                }
                slast = Some(p);
                sprites_dirty = true;
            }
            checker(sheet_r, 64);
            checker(spr_r, 16);
            for (r, src) in [
                (sheet_r, None),
                (
                    spr_r,
                    Some(Rect::new(
                        cel.x as f32,
                        cel.y as f32,
                        cel.w as f32,
                        cel.h as f32,
                    )),
                ),
            ] {
                draw_texture_ex(
                    &sprite_tex,
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
            let (sw, sh) = (side / 8.0, side / 4.0);
            for k in 0..15 {
                let (x, y) = (
                    sheet_r.x + (k % 8) as f32 * sw,
                    sheet_r.y + (k / 8) as f32 * sh,
                );
                draw_rectangle_lines(x, y, sw, sh, 1.0, Color::new(1.0, 1.0, 1.0, 0.15));
            }
            let s = skind as usize - 1;
            draw_rectangle_lines(
                sheet_r.x + (s % 8) as f32 * sw,
                sheet_r.y + (s / 8) as f32 * sh,
                sw,
                sh,
                2.0,
                YELLOW,
            );
            draw_rectangle_lines(spr_r.x, spr_r.y, spr_r.w, spr_r.h, 1.0, GRAY);
            // the trunk the car hits, from the side: a box around the foot (the slot's bottom
            // centre), 2 × radius wide and the height tall, at 10 world units per pixel
            let [r, hgt] = doc.colliders[skind as usize].map(|v| v as f32 / 10.0 * px_size);
            if r > 0.0 {
                let (fx, fy) = (spr_r.x + spr_r.w / 2.0, spr_r.bottom());
                let red = Color::new(1.0, 0.2, 0.2, 0.25);
                draw_rectangle(fx - r, fy - hgt, 2.0 * r, hgt, red);
                draw_rectangle_lines(fx - r, fy - hgt, 2.0 * r, hgt, 2.0, RED);
            }
            if on {
                let size = if matches!(t, Tool::Paint | Tool::Erase) {
                    spsize as f32
                } else {
                    1.0
                };
                let off = if size as i32 % 2 == 0 { 0.0 } else { 0.5 };
                let c = spr_r.point() + (vec2(px as f32, py as f32) + off) * px_size;
                draw_rectangle_lines(
                    c.x - size / 2.0 * px_size,
                    c.y - size / 2.0 * px_size,
                    size * px_size,
                    size * px_size,
                    1.5,
                    WHITE,
                );
            }
        }

        // drawing: the map
        if !matches!(tab, Tab::Tex | Tab::Sprites) {
            let size = vec2(SIZE as f32, SIZE as f32) * scale;
            let fade = if tab == Tab::Check { 1.0 } else { 0.35 };
            for (tex, tint) in [
                (&ground, WHITE),
                (&overlay, Color::new(1.0, 1.0, 1.0, fade)),
            ] {
                let tiles = view == View::Texture && std::ptr::eq(tex, &ground);
                if tiles {
                    // the tiles show from ~4 device pixels per cell, fully from 12
                    let px = scale * screen_dpi_scale();
                    textured.set_uniform("Detail", ((px - 4.0) / 8.0).clamp(0.0, 1.0));
                    textured.set_texture("MapTex", swatches.clone());
                    textured.set_texture("Means", means.clone());
                    gl_use_material(&textured);
                }
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
                if tiles {
                    gl_use_default_material();
                }
            }
            if let Some(v) = hovered {
                let whole = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
                let on = |c: u8| c == v || whole && c >> 4 == v >> 4;
                let rgba = doc
                    .colour
                    .iter()
                    .flat_map(|&c| [255, 255, 255, 255 * on(c) as u8]);
                update(&lit, rgba.collect());
                let a = 0.45 + 0.35 * (get_time() as f32 * 8.0).sin();
                draw_texture_ex(
                    &lit,
                    origin.x,
                    origin.y,
                    Color::new(1.0, 1.0, 1.0, a),
                    DrawTextureParams {
                        dest_size: Some(size),
                        ..Default::default()
                    },
                );
            }
            if tab == Tab::Objects {
                draw_texture_ex(
                    &objlayer,
                    origin.x,
                    origin.y,
                    WHITE,
                    DrawTextureParams {
                        dest_size: Some(size),
                        ..Default::default()
                    },
                );
                // zoomed in: the sprites themselves on their cells, rows top to bottom so the
                // nearer ones overlap (without the game's jitter)
                if scale >= 6.0 {
                    let lo = ((area.point() - origin) / scale).floor().max(Vec2::ZERO);
                    let hi = ((area.point() + area.size() - origin) / scale + 4.0)
                        .ceil()
                        .min(Vec2::splat(SIZE as f32));
                    for y in lo.y as usize..hi.y as usize {
                        for x in lo.x as usize..hi.x as usize {
                            let v = doc.objects[y * SIZE + x];
                            let Some(([sx, sy, sw, sh], foot)) = sprite_rects[(v >> 4) as usize]
                            else {
                                continue;
                            };
                            let k = objects::PIXEL * (0.5 + (v & 15) as f32 / 10.0) / 256.0 * scale;
                            let base = origin + vec2(x as f32 + 0.5, y as f32 + 0.5) * scale;
                            draw_texture_ex(
                                &sprite_tex,
                                base.x - foot[0] * k,
                                base.y - foot[1] * k,
                                WHITE,
                                DrawTextureParams {
                                    dest_size: Some(vec2(sw as f32 * k, sh as f32 * k)),
                                    source: Some(Rect::new(
                                        sx as f32, sy as f32, sw as f32, sh as f32,
                                    )),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                }
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
                    ui::text_sized(&label, p.x - d.width / 2.0, p.y + 6.0, 20.0, ink);
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
                    Tool::Scatter => outline(&obrush),
                    Tool::Erase if tab == Tab::Objects => outline(&obrush),
                    Tool::Place => outline(&Brush::PENCIL),
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
            (Tab::Height, "Heights"),
            (Tab::Colour, "Colours"),
            (Tab::Check, "Checkpoints"),
            (Tab::Tex, "Textures"),
            (Tab::Objects, "Objects"),
            (Tab::Sprites, "Sprites"),
        ] {
            if ui.button(Rect::new(x, 6.0, 104.0, 28.0), label, tab == tb) {
                tab = tb;
            }
            x += 108.0;
        }
        x += 16.0;
        let was = view;
        ui.choice(
            Rect::new(x, 6.0, 186.0, 28.0),
            &[
                (View::Colour, "colour"),
                (View::Texture, "texture"),
                (View::Heights, "heights"),
            ],
            &mut view,
        );
        ground_dirty |= view != was;
        let unsaved = doc.changed().contains(&true);
        // the map's name heads the tool column (* = unsaved changes)
        text(
            &format!("{}{}", doc.name, if unsaved { " *" } else { "" }),
            10.0,
            top + 22.0,
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
            let r = Rect::new(8.0, top + 36.0 + i as f32 * 34.0, left - 16.0, 30.0);
            if ui.button(r, &format!("{} {label}", i + 1), t == tl) {
                tool[tab as usize] = tl;
            }
        }
        // right: the tool's settings
        let (px, pw) = (w - right + 10.0, right - 20.0);
        hovered = None;
        let mut col = Column {
            x: px,
            w: pw,
            y: top + 10.0,
        };
        let name = tools(tab).iter().find(|x| x.0 == t).map_or("", |x| x.1);
        text(name, px, col.row(20.0).y + 15.0, WHITE);
        let mut size_i;
        if tab == Tab::Objects {
            // kinds: the sheet's sprites; a click picks one, Shift-click adds or drops one
            let g = col.row(pw / 2.0);
            let (cw, ch) = (g.w / 8.0, g.h / 2.0);
            for k in 1..=15u8 {
                let s = k as usize - 1;
                let r = Rect::new(
                    g.x + (s % 8) as f32 * cw,
                    g.y + (s / 8) as f32 * ch,
                    cw - 2.0,
                    ch - 2.0,
                );
                draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.2, 0.22, 0.26, 1.0));
                let c = edit::Cel::sprite(k);
                draw_texture_ex(
                    &sprite_tex,
                    r.x,
                    r.y,
                    WHITE,
                    DrawTextureParams {
                        dest_size: Some(vec2(r.w, r.h)),
                        source: Some(Rect::new(c.x as f32, c.y as f32, c.w as f32, c.h as f32)),
                        ..Default::default()
                    },
                );
                if kinds.contains(&k) {
                    draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, YELLOW);
                }
                if r.contains(mouse) && is_mouse_button_pressed(MouseButton::Left) {
                    match kinds.iter().position(|&x| x == k) {
                        _ if !shift => kinds = vec![k],
                        Some(i) if kinds.len() > 1 => _ = kinds.remove(i),
                        Some(_) => {}
                        None => kinds.push(k),
                    }
                }
            }
            text("Shift-click mixes kinds", px, col.row(18.0).y + 13.0, GRAY);
            if matches!(t, Tool::Scatter | Tool::Erase) {
                ui.slider(col.row(26.0), "brush size", &mut obj_size, 1, 32);
            }
            if t == Tool::Scatter {
                ui.slider(col.row(26.0), "density %", &mut density, 1, 100);
            }
            if matches!(t, Tool::Scatter | Tool::Place) {
                ui.slider(col.row(26.0), "size from", &mut size_lo, 0, 15);
                ui.slider(col.row(26.0), "size to", &mut size_hi, 0, 15);
            }
            let n = doc.objects.iter().filter(|&&v| v >> 4 != 0).count();
            text(
                &format!("{n} objects on the map"),
                px,
                col.row(18.0).y + 13.0,
                GRAY,
            );
            text(
                "zoom in to see them as sprites",
                px,
                col.row(18.0).y + 13.0,
                GRAY,
            );
        } else if tab == Tab::Sprites {
            let line = format!("sprite {skind} (from {} sheet)", doc.obj_from);
            text(&line, px, col.row(18.0).y + 13.0, WHITE);
            if matches!(t, Tool::Paint | Tool::Erase) {
                ui.slider(col.row(26.0), "size", &mut spsize, 1, 8);
            }
            // the sheet's own palette; colour 0 is see-through
            let g = col.row(pw);
            let c = g.w / 16.0;
            for v in 0..=255u8 {
                let (x, y) = (g.x + (v & 15) as f32 * c, g.y + (v >> 4) as f32 * c);
                if v == 0 {
                    checker(Rect::new(x, y, c, c), 2);
                } else {
                    let [r, gg, b] = doc.objpal[v as usize];
                    draw_rectangle(x, y, c, c, Color::from_rgba(r, gg, b, 255));
                }
                if v == spcolour {
                    draw_rectangle_lines(x, y, c, c, 2.0, YELLOW);
                }
            }
            if g.contains(mouse) && is_mouse_button_pressed(MouseButton::Left) {
                let q = ((mouse - g.point()) / c)
                    .as_ivec2()
                    .clamp(IVec2::ZERO, IVec2::splat(15));
                spcolour = (q.y * 16 + q.x) as u8;
            }
            if spcolour == 0 {
                text("colour 0: transparent", px, col.row(18.0).y + 13.0, GRAY);
            } else {
                // one undo step per slider drag
                let sliders = Rect::new(px, col.y, pw, 3.0 * 32.0);
                if sliders.contains(mouse) && is_mouse_button_pressed(MouseButton::Left) {
                    undo.push(doc.snap(Tab::Sprites));
                    redo.clear();
                }
                let mut rgb = doc.objpal[spcolour as usize].map(|v| v as i32);
                for (i, l) in ["red", "green", "blue"].into_iter().enumerate() {
                    ui.slider(col.row(26.0), l, &mut rgb[i], 0, 255);
                }
                let rgb = rgb.map(|v| v as u8);
                if rgb != doc.objpal[spcolour as usize] {
                    doc.objpal[spcolour as usize] = rgb;
                    sprites_dirty = true;
                }
            }
            let r = col.row(28.0);
            let half = (r.w - 4.0) / 2.0;
            let cel = edit::Cel::sprite(skind);
            if ui.button(Rect::new(r.x, r.y, half, r.h), "Copy sprite", false) {
                sclip = Some(cel.copy(&doc.objtex));
            }
            if ui.button(
                Rect::new(r.x + half + 4.0, r.y, half, r.h),
                "Paste sprite",
                false,
            ) && let Some(c) = &sclip
            {
                undo.push(doc.snap(Tab::Sprites));
                redo.clear();
                cel.paste(&mut doc.objtex, c);
                sprites_dirty = true;
            }
            // what a car hits (OBJTEX.TXT, saved with the sheet); the red box on the sprite
            col.row(4.0);
            text("collider (red box)", px, col.row(18.0).y + 13.0, GRAY);
            let area_r = Rect::new(px, col.y, pw, 3.0 * 32.0);
            if area_r.contains(mouse) && is_mouse_button_pressed(MouseButton::Left) {
                undo.push(doc.snap(Tab::Sprites));
                redo.clear();
            }
            let [r0, h0] = doc.colliders[skind as usize];
            let mut solid = r0 > 0;
            ui.choice(
                col.row(26.0),
                &[(true, "solid"), (false, "drive through")],
                &mut solid,
            );
            let (mut r, mut hgt) = (r0 as i32, h0 as i32);
            if solid {
                // switched back on: a thin trunk as tall as the sprite's slot
                if r == 0 {
                    (r, hgt) = (15, hgt.max(640));
                }
                ui.slider(col.row(26.0), "trunk radius", &mut r, 1, 320);
                ui.slider(col.row(26.0), "height", &mut hgt, 10, 1280);
            } else {
                r = 0;
            }
            doc.colliders[skind as usize] = [r as u16, hgt as u16];
        } else if tab == Tab::Tex {
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
                clip = Some(edit::Cel::tile(tile).copy(&doc.maptex));
            }
            if ui.button(
                Rect::new(r.x + half + 4.0, r.y, half, r.h),
                "Paste tile",
                false,
            ) && let Some(c) = &clip
            {
                undo.push(doc.snap(Tab::Tex));
                redo.clear();
                edit::Cel::tile(tile).paste(&mut doc.maptex, c);
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
                        hovered = Some(v);
                        if is_mouse_button_pressed(MouseButton::Left) {
                            colour = v;
                            if t == Tool::Pick {
                                tool[tab as usize] = Tool::Paint;
                            }
                        }
                    }
                    // the colour whose dirt digs ruts: a brown dot in its corner
                    let (dx, dy) = (
                        g.x + (DIG_COLOUR & 15) as f32 * tile,
                        g.y + (DIG_COLOUR >> 4) as f32 * tile,
                    );
                    draw_circle(
                        dx + tile - 5.0,
                        dy + 5.0,
                        3.5,
                        Color::new(0.45, 0.25, 0.1, 1.0),
                    );
                    draw_circle_lines(dx + tile - 5.0, dy + 5.0, 3.5, 1.0, BLACK);
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
                    // what the material does to a car (the sim's tables), against the best of
                    // each: grip (slip held before sliding), load (weight that counts towards
                    // grip), drag (speed lost per tick on contact)
                    let m = (shown >> 4) as usize;
                    let bars = [
                        (
                            "grip",
                            GRIP.iter().min().copied().unwrap() as f32 / GRIP[m] as f32,
                        ),
                        (
                            "load",
                            MAX_LOAD[m] as f32 / *MAX_LOAD.iter().max().unwrap() as f32,
                        ),
                        (
                            "drag",
                            0.5f32.powi((DAMP[m] - DAMP.iter().min().unwrap()) as i32),
                        ),
                    ];
                    for (label, t) in bars {
                        let r = col.row(16.0);
                        draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.2, 0.21, 0.24, 1.0));
                        draw_rectangle(r.x, r.y, r.w * t, r.h, Color::new(0.45, 0.36, 0.08, 1.0));
                        let pct = (t * 100.0).round();
                        text(&format!("{label}: {pct}%"), r.x + 6.0, r.y + 12.5, WHITE);
                    }
                    if shown == DIG_COLOUR {
                        text("sliding here digs ruts", px, col.row(18.0).y + 13.0, GRAY);
                    }
                    text(
                        "Shift: light up the whole material",
                        px,
                        col.row(18.0).y + 13.0,
                        GRAY,
                    );
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
                    (
                        ground_dirty,
                        check_dirty,
                        swatch_dirty,
                        objects_dirty,
                        sprites_dirty,
                    ) = (true, true, true, true, true);
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
        if on_map && tab == Tab::Objects {
            let v = doc.objects[cell];
            let what = if v >> 4 == 0 {
                "nothing".to_string()
            } else {
                format!("kind {}, size {}", v >> 4, v & 15)
            };
            text(&format!("({cx}, {cy})   {what}"), 10.0, h - 8.0, WHITE);
        } else if on_map && !matches!(tab, Tab::Tex | Tab::Sprites) {
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
            ui::text_sized(&note.0, nx, ny, 22.0, YELLOW);
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
            ui::text_sized(format!("{filter}_"), r.x + 16.0, r.y + 60.0, 26.0, YELLOW);
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
            ui::text_sized(format!("{name}_"), r.x + 16.0, r.y + 68.0, 26.0, YELLOW);
            text("Enter: ok   Esc: cancel", r.x + 16.0, r.y + 100.0, GRAY);
        }

        // another map in the window: opened, created or saved as
        if let Some(name) = open.take() {
            ask = None;
            match Doc::load(&name) {
                Ok(d) => {
                    doc = d;
                    remember_map(&name);
                    (undo, redo, selected) = (vec![], vec![], None);
                    (
                        ground_dirty,
                        check_dirty,
                        swatch_dirty,
                        objects_dirty,
                        sprites_dirty,
                    ) = (true, true, true, true, true);
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
