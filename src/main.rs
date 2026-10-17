//! Reformers, the game: `cargo run --release [-- --soft] [--car N]`.
//! Arrows drive, Space brakes, R puts the car back on its wheels (on the last checkpoint in a time
//! trial), 1–5 switch car, W toggles the spring wireframe, C cycles the camera, Esc opens the
//! options overlay (resume, map, camera, graphics, quit). `--map NAME` picks a map folder. With `CHECK.PCX` present the map is a time trial: the timer starts at the start line.
//! `--menu` starts with the overlay open; `--smooth` / `--untextured` / `--crt` preset the graphics options.
//! `--shot out.png`: hold throttle + left, save a frame after 4 s and exit (for checking without a human).

use macroquad::audio::{
    PlaySoundParams, Sound, load_sound_from_bytes, play_sound, set_sound_volume,
};
use macroquad::miniquad::{BlendFactor, BlendState, BlendValue, Equation};
use macroquad::prelude::*;
use reformers::car::{self, Car, Prim};
use reformers::ghost::{self, Frame, Ghost};
use reformers::sim::{Keys, Sim, SimCar, TICK_HZ};
use reformers::terrain::{SIZE, Terrain};
use reformers::track::{Event, Lap, Race, Track, Zone, format_time};
use reformers::{assets, pcx, sound};
use std::path::PathBuf;

const S: f32 = 256.0; // world units per render unit (one terrain cell)
const CHUNK: usize = 32;
const CAM_CLEARANCE: f32 = 0.3; // render units the camera keeps above the ground // rows per mesh: macroquad meshes index with u16
const SPAWN: (u16, u16) = (0x8000, 0x7a00); // without CHECK.PCX

fn data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let base = env("XDG_DATA_HOME")
        .or_else(|| env("HOME").map(|h| h.join(".local/share")))
        .or_else(|| env("APPDATA"))?;
    Some(base.join("reformers"))
}

/// Engine loop pitches, ×1.25 apart around the original's idle note; crossfaded by speed.
const ENGINE_STEPS: [f32; 7] = [0.75, 0.94, 1.17, 1.46, 1.83, 2.29, 2.86];

/// The game's sounds, synthesised by `sound` at start-up.
struct Audio {
    engine: Vec<Sound>, // one looping tone per ENGINE_STEPS
    skid: Sound,
    thud: Sound,
    checkpoint: Sound,
    lap: Sound,
    best: Sound,
}

impl Audio {
    async fn load() -> Option<Audio> {
        async fn load(samples: Vec<f32>) -> Option<Sound> {
            load_sound_from_bytes(&sound::wav(&samples)).await.ok()
        }
        let mut engine = vec![];
        for k in ENGINE_STEPS {
            engine.push(load(sound::engine_loop(sound::engine_idle_hz() * k)).await?);
        }
        let audio = Audio {
            engine,
            skid: load(sound::skid_loop()).await?,
            thud: load(sound::thud()).await?,
            checkpoint: load(sound::chime(&[(880.0, 0.3)])).await?,
            lap: load(sound::chime(&[(660.0, 0.15), (990.0, 0.4)])).await?,
            best: load(sound::chime(&[(660.0, 0.12), (880.0, 0.12), (1320.0, 0.5)])).await?,
        };
        for s in audio.engine.iter().chain([&audio.skid]) {
            play_sound(
                s,
                PlaySoundParams {
                    looped: true,
                    volume: 0.0,
                },
            );
        }
        Some(audio)
    }

    /// Engine at `pitch` × the idle note (louder on the throttle), skid at `skid` 0–1, scaled by
    /// `vol`; `vol` 0 silences the loops (paused or muted).
    fn update(&self, pitch: f32, throttle: bool, skid: f32, vol: f32) {
        let at = ENGINE_STEPS
            .iter()
            .position(|&k| k > pitch)
            .unwrap_or(ENGINE_STEPS.len() - 1)
            .max(1);
        let (lo, hi) = (ENGINE_STEPS[at - 1], ENGINE_STEPS[at]);
        let w = ((pitch / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0);
        let loud = vol * if throttle { 0.4 } else { 0.18 };
        for (i, s) in self.engine.iter().enumerate() {
            let g = if i == at - 1 {
                1.0 - w
            } else if i == at {
                w
            } else {
                0.0
            };
            set_sound_volume(s, loud * g);
        }
        set_sound_volume(&self.skid, vol * 0.5 * skid);
    }

    fn chime(&self, ev: &Option<Event>, vol: f32) {
        let s = match ev {
            Some(Event::Start | Event::Gate(_)) => &self.checkpoint,
            Some(Event::Lap { best: true, .. }) => &self.best,
            Some(Event::Lap { .. }) => &self.lap,
            None => return,
        };
        play_sound(
            s,
            PlaySoundParams {
                looped: false,
                volume: vol,
            },
        );
    }
}

fn settings_file() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.txt"))
}

fn save_settings(gfx: &Graphics) {
    let Some(path) = settings_file() else { return };
    let written = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::write(&path, gfx.to_text()));
    if let Err(e) = written {
        eprintln!("can't save settings to {}: {e}", path.display());
    }
}

/// Best laps, one `key time split…` line per map + car, in the user's data dir.
fn best_laps_file() -> Option<PathBuf> {
    Some(data_dir()?.join("best_laps.txt"))
}

/// The best lap's ghost for this map + car; empty if there is none (or it won't load).
fn load_ghost(level: &Level, car: usize) -> Ghost {
    best_key(level, car)
        .and_then(|k| std::fs::read(data_dir()?.join(format!("ghost-{k}.bin"))).ok())
        .and_then(|b| Ghost::from_bytes(&b).ok())
        .unwrap_or_default()
}

fn save_ghost(key: &str, ghost: &Ghost) {
    let Some(dir) = data_dir() else { return };
    let written = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join(format!("ghost-{key}.bin")), ghost.to_bytes()));
    if let Err(e) = written {
        eprintln!("can't save ghost to {}: {e}", dir.display());
    }
}

fn load_best(key: &str) -> Option<Lap> {
    let text = std::fs::read_to_string(best_laps_file()?).ok()?;
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != key {
            return None;
        }
        let mut nums = words.map(|w| w.parse::<u64>().ok());
        Some(Lap {
            time: nums.next()??,
            splits: nums.collect::<Option<_>>()?,
        })
    })
}

fn save_best(key: &str, lap: &Lap) {
    let Some(path) = best_laps_file() else { return };
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = old
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(key))
        .map(String::from)
        .collect();
    let splits: Vec<String> = lap.splits.iter().map(u64::to_string).collect();
    lines.push(format!("{key} {} {}", lap.time, splits.join(" ")));
    let written = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::write(&path, lines.join("\n") + "\n"));
    if let Err(e) = written {
        eprintln!("can't save best lap to {}: {e}", path.display());
    }
}

/// The map: what MAP.PCX + COL.PCX + the optional CHECK.PCX describe beyond the terrain itself.
struct Level {
    track: Option<Track>,
    /// Identifies these exact files, so best laps belong to one track; None without a time trial.
    key: Option<u64>,
    /// (x, y, gate) for every painted gate cell.
    gate_cells: Vec<(usize, usize, usize)>,
    /// (x, y, zone) for every painted zone cell.
    zone_cells: Vec<(usize, usize, Zone)>,
    /// MAPTEX tile sheet for this map's palette: the map folder's own, or the shared one when
    /// the palettes match (TEREP1's BMP palette doesn't, so it draws flat colours).
    maptex: Option<Vec<u8>>,
}

/// A map's files whose changes reload it while the game runs.
fn watched(map: &str) -> Vec<Option<std::time::SystemTime>> {
    ["MAP.PCX", "MAP.BMP", "COL.PCX", "COL.BMP", "CHECK.PCX"]
        .iter()
        .map(|f| assets::modified(&format!("maps/{map}/{f}")))
        .collect()
}

/// `--map NAME`, else the map saved in settings.txt, else terep2, else the first map folder.
fn pick_map(args: &[String], saved: &str) -> String {
    let maps = assets::maps();
    let flag = args
        .iter()
        .position(|a| a == "--map")
        .and_then(|i| args.get(i + 1));
    if let Some(m) = flag {
        if !maps.contains(m) {
            panic!("no map {m:?}; maps: {}", maps.join(", "));
        }
        return m.clone();
    }
    let saved = saved
        .lines()
        .find_map(|l| l.strip_prefix("map="))
        .map(str::trim);
    [saved, Some("terep2")]
        .into_iter()
        .flatten()
        .find(|m| maps.iter().any(|x| x == m))
        .map(String::from)
        .or_else(|| maps.first().cloned())
        .unwrap_or_else(|| panic!("no maps found: put map folders in assets/maps/"))
}

fn load_level(name: &str) -> Result<(Terrain, Level), String> {
    let (map_file, map) = assets::read_map_image(name, "MAP")?;
    let (col_file, col) = assets::read_map_image(name, "COL")?;
    let terrain = Terrain::load(&map, &col).map_err(|e| format!("{map_file}/{col_file}: {e}"))?;
    let check = assets::read(&format!("maps/{name}/CHECK.PCX")).ok();
    let maptex = assets::read(&format!("maps/{name}/MAPTEX.PCX"))
        .ok()
        .or_else(|| {
            let shared = assets::read("MAPTEX.PCX").ok()?;
            (pcx::decode(&shared).ok()?.palette == terrain.palette).then_some(shared)
        });
    let track = check.as_deref().map(Track::load).transpose()?;
    let gate_cells = track.as_ref().map_or(vec![], |t| {
        (0..SIZE * SIZE)
            .filter_map(|i| {
                t.gate_at(i % SIZE, i / SIZE)
                    .map(|g| (i % SIZE, i / SIZE, g))
            })
            .collect()
    });
    let zone_cells = track.as_ref().map_or(vec![], |t| {
        (0..SIZE * SIZE)
            .filter_map(|i| {
                t.zone_at(i % SIZE, i / SIZE)
                    .map(|z| (i % SIZE, i / SIZE, z))
            })
            .collect()
    });
    let level = Level {
        track,
        key: check.map(|c| track_hash(&[&map, &col, &c])),
        gate_cells,
        zone_cells,
        maptex,
    };
    Ok((terrain, level))
}

fn best_key(level: &Level, car: usize) -> Option<String> {
    level.key.map(|k| format!("{k:016x}-car{}", car + 1))
}

/// FNV-1a over the map files: best laps belong to this exact track.
fn track_hash(files: &[&[u8]]) -> u64 {
    files
        .iter()
        .flat_map(|f| f.iter())
        .fold(0xcbf29ce484222325, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        })
}

/// Which way the car points, radians clockwise from map-up (`SimCar::spawn_facing`'s convention).
fn heading(car: &SimCar) -> f32 {
    let a = car.atom_positions();
    let fwd = [
        a[6][0] + a[7][0] - a[4][0] - a[5][0],
        a[6][1] + a[7][1] - a[4][1] - a[5][1],
    ];
    fwd[0].atan2(-fwd[1])
}

/// World (x, y, z-up) → render space (y-up). The world is left-handed (PCX rows run down),
/// so y maps to +z; mapping it to −z would show the map mirrored.
fn v(p: [f32; 3]) -> Vec3 {
    vec3(p[0] / S, p[2] / S, p[1] / S)
}

/// COL.PCX, one texel per terrain cell, through the palette.
fn colour_image(t: &Terrain) -> Image {
    let bytes = t
        .colour
        .iter()
        .flat_map(|&i| {
            let [r, g, b] = t.palette[i as usize];
            [r, g, b, 255]
        })
        .collect();
    Image {
        bytes,
        width: SIZE as u16,
        height: SIZE as u16,
    }
}

/// COL.PCX raw indices (in red) for the textured shader, which picks the MAPTEX tile from them.
fn index_image(t: &Terrain) -> Image {
    let bytes = t.colour.iter().flat_map(|&i| [i, 0, 0, 255]).collect();
    Image {
        bytes,
        width: SIZE as u16,
        height: SIZE as u16,
    }
}

/// The map's MAPTEX tiles, or a white texel for maps without matching tiles (drawn flat then).
fn tiles(level: &Level, palette: &[[u8; 3]; 256]) -> Texture2D {
    level.maptex.as_ref().map_or_else(
        || Texture2D::from_rgba8(1, 1, &[255, 255, 255, 255]),
        |b| maptex_texture(b, palette),
    )
}

/// MAPTEX.PCX's 256×256 tile sheet through the palette: row = colour >> 4, column = colour & 15,
/// each tile a 16×16 noisy swatch of that exact colour.
fn maptex_texture(pcx_bytes: &[u8], palette: &[[u8; 3]; 256]) -> Texture2D {
    let img = pcx::decode(pcx_bytes).expect("MAPTEX.PCX").crop(SIZE, SIZE);
    Texture2D::from_rgba8(
        SIZE as u16,
        SIZE as u16,
        &img.pixels
            .iter()
            .flat_map(|&i| {
                let [r, g, b] = palette[i as usize];
                [r, g, b, 255]
            })
            .collect::<Vec<_>>(),
    )
}

const TERRAIN_VERTEX: &str = "#version 100
attribute vec3 position;
attribute vec2 texcoord;
attribute vec4 color0;
varying lowp vec2 uv;
varying lowp vec4 shade;
varying mediump float fog;
uniform mat4 Model;
uniform mat4 Projection;
uniform vec2 FogRange;
void main() {
    gl_Position = Projection * Model * vec4(position, 1);
    uv = texcoord;
    shade = color0 / 255.0;
    fog = clamp((gl_Position.w - FogRange.x) / (FogRange.y - FogRange.x), 0.0, 1.0);
}
";

/// Textured terrain: the cell's COL index (alpha) selects a MAPTEX tile, tiled once per cell.
const TERRAIN_FRAGMENT: &str = "#version 100
precision mediump float;
varying lowp vec2 uv;
varying lowp vec4 shade;
varying mediump float fog;
uniform sampler2D ColIdx;  // COL indices in red, nearest
uniform sampler2D MapTex;
uniform vec3 FogColor;
void main() {
    vec2 cell = uv * 256.0;
    float idx = floor(texture2D(ColIdx, (floor(cell) + 0.5) / 256.0).r * 255.0 + 0.5);
    vec2 tile = vec2(mod(idx, 16.0), floor(idx / 16.0));
    vec2 st = (tile * 16.0 + 0.5 + fract(cell) * 15.0) / 256.0;  // stay inside the tile
    gl_FragColor = vec4(mix(texture2D(MapTex, st).rgb * shade.rgb, FogColor, fog), 1.0);
}
";

/// Everything else in 3D (flat terrain, cars, arches, pads, dirt): macroquad's default look plus fog.
const FOG_FRAGMENT: &str = "#version 100
precision mediump float;
varying lowp vec2 uv;
varying lowp vec4 shade;
varying mediump float fog;
uniform sampler2D Texture;
uniform vec3 FogColor;
void main() {
    vec4 c = texture2D(Texture, uv) * shade;
    gl_FragColor = vec4(mix(c.rgb, FogColor, fog), c.a);
}
";

/// FOG.DAT: past ~10 cells the original remaps colours toward this sky blue, fully by ~34
/// (table index = average depth / 512 world units; levels 5–17 change colours).
const FOG_COLOUR: [f32; 3] = [92.0 / 255.0, 188.0 / 255.0, 252.0 / 255.0];
const FOG_RANGE: [f32; 2] = [10.0, 34.0];

fn fog_material(vertex: &'static str, fragment: &'static str, textures: Vec<String>) -> Material {
    load_material(
        ShaderSource::Glsl { vertex, fragment },
        MaterialParams {
            textures,
            uniforms: vec![
                UniformDesc::new("FogColor", UniformType::Float3),
                UniformDesc::new("FogRange", UniformType::Float2),
            ],
            pipeline_params: PipelineParams {
                depth_test: Comparison::LessOrEqual,
                depth_write: true,
                color_blend: Some(BlendState::new(
                    Equation::Add,
                    BlendFactor::Value(BlendValue::SourceAlpha),
                    BlendFactor::OneMinusValue(BlendValue::SourceAlpha),
                )),
                ..Default::default()
            },
        },
    )
    .expect("fog shader")
}

/// Terrain rows for one chunk. Colour comes from the COL texture (uv at the cell centre);
/// the vertex colour carries only the slope shading, which macroquad multiplies in.
fn chunk_mesh(t: &Terrain, chunk: usize) -> Mesh {
    let h = |x: usize, y: usize| t.height[y.min(SIZE - 1) * SIZE + x.min(SIZE - 1)] as f32 * 16.0;
    let light = vec3(-0.4, 1.0, 0.3).normalize();
    let rows = chunk * CHUNK..((chunk + 1) * CHUNK + 1).min(SIZE);
    let mut vertices = Vec::new();
    for y in rows.clone() {
        for x in 0..SIZE {
            let n = vec3(
                h(x.saturating_sub(1), y) - h(x + 1, y),
                2.0 * S,
                h(x, y.saturating_sub(1)) - h(x, y + 1),
            )
            .normalize();
            let shade = 0.35 + 0.65 * n.dot(light).max(0.0);
            let (u, w) = (
                (x as f32 + 0.5) / SIZE as f32,
                (y as f32 + 0.5) / SIZE as f32,
            );
            vertices.push(Vertex::new(
                x as f32,
                h(x, y) / S,
                y as f32,
                u,
                w,
                Color::new(shade, shade, shade, 1.0),
            ));
        }
    }
    let mut indices = Vec::new();
    for r in 0..rows.len() - 1 {
        for x in 0..SIZE - 1 {
            let i = (r * SIZE + x) as u16;
            let (right, down) = (i + 1, i + SIZE as u16);
            indices.extend([i, down, right, right, down, down + 1]);
        }
    }
    Mesh {
        vertices,
        indices,
        texture: None,
    }
}

/// A car skin: the GPU texture plus the indexed image for untextured colours.
struct Skin {
    texture: Texture2D,
    image: pcx::Image,
}

/// CARn.PCX through the game palette (the original only ever sets COL.PCX's palette).
/// The most common index is the background and becomes transparent.
fn skin_texture(pcx_bytes: &[u8], palette: &[[u8; 3]; 256]) -> Skin {
    let img = pcx::decode(pcx_bytes).expect("car skin");
    let mut count = [0usize; 256];
    img.pixels.iter().for_each(|&i| count[i as usize] += 1);
    let bg = (0..256).max_by_key(|&i| count[i]).unwrap() as u8;
    let rgba: Vec<u8> = img
        .pixels
        .iter()
        .flat_map(|&i| {
            let [r, g, b] = palette[i as usize];
            [r, g, b, if i == bg { 0 } else { 255 }]
        })
        .collect();
    let tex = Texture2D::from_rgba8(img.width as u16, img.height as u16, &rgba);
    tex.set_filter(FilterMode::Nearest);
    Skin {
        texture: tex,
        image: img,
    }
}

/// Builds the car's meshes for this frame from the display-list primitives.
/// `roll` is each wheel's accumulated rotation (radians), by atom index 0–3.
#[allow(clippy::too_many_arguments)]
fn car_meshes(
    model: &Car,
    atoms: &[[f32; 3]],
    steer: f32,
    palette: &[[u8; 3]; 256],
    t: &Terrain,
    skin: &Skin,
    roll: &[f32; 4],
    use_skin: bool,
) -> [Mesh; 3] {
    let n = atoms.len();
    let w = |p: [f32; 3]| Vec3::from(p);
    let unit = |a: Vec3| a.normalize_or_zero();
    let side = unit(w(atoms[7]) - w(atoms[6]));
    let fwd = unit(w(atoms[4]) - w(atoms[6]) + w(atoms[5]) - w(atoms[7]));
    let steered = fwd * steer.sin() + side * steer.cos(); // like 0x11f0
    let entry = |e: u16| -> Vec3 {
        let e = e as usize;
        let world = if e < 2 * n {
            let [x, y, z] = atoms[e / 2];
            if e % 2 == 1 {
                vec3(x, y, t.height_at(x, y) + 3.0)
            } else {
                vec3(x, y, z)
            }
        } else if e == 2 * n {
            w(atoms[0]) + steered * 128.0
        } else {
            w(atoms[1]) - steered * 128.0
        };
        v(world.into())
    };
    let skin_size = vec2(skin.texture.width(), skin.texture.height());
    // untextured: each textured face / wheel takes the skin colour at its uv centre
    let mid = |uvs: &mut dyn Iterator<Item = [f32; 2]>| -> Color {
        let (mut sum, mut n) = ([0.0f32; 2], 0.0);
        uvs.for_each(|uv| {
            sum[0] += uv[0];
            sum[1] += uv[1];
            n += 1.0;
        });
        let (x, y) = ((sum[0] / n) as usize, (sum[1] / n) as usize);
        let i = skin.image.pixels
            [y.min(skin.image.height - 1) * skin.image.width + x.min(skin.image.width - 1)];
        let [r, g, b] = palette[i as usize];
        Color::from_rgba(r, g, b, 255)
    };
    let mut flat = Mesh {
        vertices: vec![],
        indices: vec![],
        texture: None,
    };
    let tex = use_skin.then(|| skin.texture.clone());
    let mut textured = Mesh {
        vertices: vec![],
        indices: vec![],
        texture: tex.clone(),
    };
    let mut wheels = Mesh {
        vertices: vec![],
        indices: vec![],
        texture: tex,
    };
    let fan = |m: &mut Mesh, verts: Vec<Vertex>| {
        let base = m.vertices.len() as u16;
        for i in 1..verts.len().saturating_sub(1) as u16 {
            m.indices.extend([base, base + i, base + i + 1]);
        }
        m.vertices.extend(verts);
    };
    for prim in &model.prims {
        match prim {
            Prim::Flat { verts, colour } => {
                let shadow = verts.iter().all(|e| e % 2 == 1);
                let c = if shadow {
                    Color::from_rgba(0, 0, 0, 110)
                } else if *colour >= 0xf0 {
                    // SMOKE.DAT: colours f0–f3 remap what's behind (0x38f8): darken a lot / a
                    // little, lighten a little / a lot. Windows are f0.
                    [
                        Color::new(0.0, 0.0, 0.0, 0.36),
                        Color::new(0.0, 0.0, 0.0, 0.15),
                        Color::new(1.0, 1.0, 1.0, 0.08),
                        Color::new(1.0, 1.0, 1.0, 0.2),
                    ][(*colour as usize - 0xf0).min(3)]
                } else {
                    let [r, g, b] = palette[*colour as usize];
                    Color::from_rgba(r, g, b, 255)
                };
                fan(
                    &mut flat,
                    verts
                        .iter()
                        .map(|&e| {
                            let p = entry(e);
                            Vertex::new(p.x, p.y, p.z, 0.0, 0.0, c)
                        })
                        .collect(),
                );
            }
            Prim::Textured { verts } => {
                let panel = if use_skin {
                    WHITE
                } else {
                    mid(&mut verts.iter().map(|v| v.1))
                };
                fan(
                    &mut textured,
                    verts
                        .iter()
                        .map(|&(e, uv)| {
                            let p = entry(e);
                            Vertex::new(
                                p.x,
                                p.y,
                                p.z,
                                uv[0] / skin_size.x,
                                uv[1] / skin_size.y,
                                panel,
                            )
                        })
                        .collect(),
                );
            }
            Prim::Wheel {
                centre,
                axle,
                frames,
            } => {
                let i = (*centre / 2) as usize;
                let c = entry(*centre);
                let a = unit(entry(*axle) - c);
                let t1 = unit(a.cross(Vec3::Y));
                let t2 = a.cross(t1);
                let (sr, cr) = roll[i.min(3)].sin_cos();
                let (r1, r2) = (t1 * cr + t2 * sr, t2 * cr - t1 * sr);
                let r = model.atoms[i].radius / S;
                if use_skin {
                    let corners = [-r1 - r2, r1 - r2, r1 + r2, -r1 + r2];
                    fan(
                        &mut wheels,
                        (0..4)
                            .map(|k| {
                                let p = c + corners[k] * r;
                                let uv = frames[0][k];
                                Vertex::new(
                                    p.x,
                                    p.y,
                                    p.z,
                                    uv[0] / skin_size.x,
                                    uv[1] / skin_size.y,
                                    WHITE,
                                )
                            })
                            .collect(),
                    );
                } else {
                    // a disc in the tyre's colour (sampled an eighth in from the frame's edge, past the rim)
                    let [a0, _, b0, _] = frames[0];
                    let tyre = mid(
                        &mut [[a0[0] * 0.875 + b0[0] * 0.125, (a0[1] + b0[1]) / 2.0]].into_iter(),
                    );
                    fan(
                        &mut wheels,
                        (0..12)
                            .map(|k| {
                                let (sa, ca) = (k as f32 * std::f32::consts::TAU / 12.0).sin_cos();
                                let p = c + (r1 * ca + r2 * sa) * r;
                                Vertex::new(p.x, p.y, p.z, 0.0, 0.0, tyre)
                            })
                            .collect(),
                    );
                }
            }
        }
    }
    [flat, textured, wheels]
}

/// Checkpoint posts reach this many cells above the highest ground on the gate's line.
const GATE_HEIGHT: f32 = 2.0;

/// A checkpoint arch: a post at each end of the line, from the ground to `top` (world z), and a
/// banner between their tops; start/finish gets a chequered one. Dim and translucent unless `next`.
fn draw_gate(t: &Terrain, ends: [[f32; 2]; 2], top: f32, start: bool, next: bool, cam: Vec3) {
    // fade out within ~2 cells of the camera, which passes right through the banner
    let (a, b) = (
        v([ends[0][0], ends[0][1], 0.0]).xz(),
        v([ends[1][0], ends[1][1], 0.0]).xz(),
    );
    let ab = b - a;
    let f = ((cam.xz() - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    let near = ((cam.xz().distance(a + ab * f) - 0.8) / 1.5).clamp(0.0, 1.0);
    let alpha = if next { 1.0 } else { 0.3 } * near;
    let post = Color::new(0.55, 0.55, 0.58, alpha);
    for [x, y] in ends {
        let ground = t.height_at(x, y);
        draw_cylinder(
            v([x, y, ground]),
            0.12,
            0.12,
            (top - ground) / S,
            None,
            post,
        );
    }
    // banner: a strip of quads 0.6 cells tall just under the post tops; start/finish is a chequer of
    // 0.1-cell rows and 1/6-cell columns
    let ([ax, ay], [bx, by]) = (ends[0], ends[1]);
    let cols = if start {
        (((bx - ax).hypot(by - ay) / S) * 6.0).ceil().max(1.0) as usize
    } else {
        1
    };
    let mut mesh = Mesh {
        vertices: vec![],
        indices: vec![],
        texture: None,
    };
    let rows = if start { 6 } else { 1 };
    let row_h = 0.6 / rows as f32;
    for row in 0..rows {
        for col in 0..cols {
            let colour = match (start, (row + col) % 2) {
                (true, 0) => Color::new(0.05, 0.05, 0.05, alpha),
                (true, _) => Color::new(1.0, 1.0, 1.0, alpha),
                (false, _) if next => Color::new(1.0, 0.85, 0.1, 1.0),
                (false, _) => Color::new(1.0, 1.0, 1.0, alpha * 0.8),
            };
            let (f0, f1) = (col as f32 / cols as f32, (col + 1) as f32 / cols as f32);
            let (z0, z1) = (
                top - (0.7 - row as f32 * row_h) * S,
                top - (0.7 - (row + 1) as f32 * row_h) * S,
            );
            let at = |f: f32, z: f32| {
                let p = v([ax + (bx - ax) * f, ay + (by - ay) * f, z]);
                Vertex::new(p.x, p.y, p.z, 0.0, 0.0, colour)
            };
            let base = mesh.vertices.len() as u16;
            mesh.vertices
                .extend([at(f0, z0), at(f1, z0), at(f1, z1), at(f0, z1)]);
            mesh.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    draw_mesh(&mesh);
}

const RESOLUTIONS: [(f32, f32); 4] = [
    (960.0, 600.0),
    (1280.0, 800.0),
    (1600.0, 1000.0),
    (1920.0, 1200.0),
];

const CAMERAS: [&str; 4] = ["chase", "bumper", "orbit (Q/E)", "top-down"];

/// Graphics options: one field + one menu row each; saved to settings.txt.
struct Graphics {
    /// Map folder name (switching it in the menu loads that map).
    map: String,
    camera: usize, // index into CAMERAS
    nearest: bool,
    textured: bool,
    crt: bool,
    minimap: bool,
    ghost: bool,
    fog: bool,
    volume: u8, // 0 (mute) – 10
    fullscreen: bool,
    resolution: usize, // index into RESOLUTIONS, windowed mode only
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Resume,
    Map,
    Sound,
    Camera,
    Filter,
    Textures,
    Crt,
    Fog,
    Minimap,
    Ghost,
    Fullscreen,
    Resolution,
    Quit,
}
const ROWS: [Row; 13] = [
    Row::Resume,
    Row::Map,
    Row::Sound,
    Row::Camera,
    Row::Filter,
    Row::Textures,
    Row::Crt,
    Row::Fog,
    Row::Minimap,
    Row::Ghost,
    Row::Fullscreen,
    Row::Resolution,
    Row::Quit,
];

impl Graphics {
    fn to_text(&self) -> String {
        format!(
            "map={}\ncamera={}\nnearest={}\ntextured={}\ncrt={}\nminimap={}\nghost={}\nfog={}\nvolume={}\nfullscreen={}\nresolution={}\n",
            self.map,
            self.camera,
            self.nearest,
            self.textured,
            self.crt,
            self.minimap,
            self.ghost,
            self.fog,
            self.volume,
            self.fullscreen,
            self.resolution
        )
    }

    /// Applies `key=value` lines from `to_text`; unknown keys and bad values are skipped.
    fn apply_text(&mut self, text: &str) {
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let (b, n) = (v.trim() == "true", v.trim().parse::<usize>().ok());
            match k.trim() {
                "map" => self.map = v.trim().to_string(),
                "camera" => self.camera = n.filter(|&n| n < CAMERAS.len()).unwrap_or(self.camera),
                "nearest" => self.nearest = b,
                "textured" => self.textured = b,
                "crt" => self.crt = b,
                "minimap" => self.minimap = b,
                "ghost" => self.ghost = b,
                "fog" => self.fog = b,
                "volume" => self.volume = n.filter(|&n| n <= 10).map_or(self.volume, |n| n as u8),
                "fullscreen" => self.fullscreen = b,
                "resolution" => {
                    self.resolution = n
                        .filter(|&n| n < RESOLUTIONS.len())
                        .unwrap_or(self.resolution)
                }
                _ => {}
            }
        }
    }

    fn label(&self, row: Row) -> String {
        let on = |b: bool| if b { "on" } else { "off" };
        match row {
            Row::Resume => "Resume".into(),
            Row::Map => format!("Map: {}", self.map),
            Row::Sound => match self.volume {
                0 => "Sound: off".into(),
                v => format!("Sound: {v}/10"),
            },
            Row::Camera => format!("Camera: {}  (C)", CAMERAS[self.camera]),
            Row::Filter => format!(
                "Texture filter: {}",
                if self.nearest { "nearest" } else { "smooth" }
            ),
            Row::Textures => format!("Textures: {}", on(self.textured)),
            Row::Crt => format!("CRT filter: {}", on(self.crt)),
            Row::Minimap => format!("Minimap: {}", on(self.minimap)),
            Row::Fog => format!("Fog: {}", on(self.fog)),
            Row::Ghost => format!("Ghost car: {}", on(self.ghost)),
            Row::Fullscreen => format!("Fullscreen: {}", on(self.fullscreen)),
            Row::Resolution => {
                let (w, h) = RESOLUTIONS[self.resolution];
                format!(
                    "Resolution: {w}x{h}{}",
                    if self.fullscreen {
                        "  (windowed only)"
                    } else {
                        ""
                    }
                )
            }
            Row::Quit => "Quit".into(),
        }
    }

    /// Changes a row's value; `step` is ±1 for left/right, 0 for Enter. Returns false for rows without a value.
    fn change(&mut self, row: Row, step: i32) -> bool {
        match row {
            Row::Filter => self.nearest = !self.nearest,
            Row::Textures => self.textured = !self.textured,
            Row::Crt => self.crt = !self.crt,
            Row::Minimap => self.minimap = !self.minimap,
            Row::Fog => self.fog = !self.fog,
            Row::Sound => {
                self.volume =
                    (self.volume as i32 + if step == 0 { 1 } else { step }).rem_euclid(11) as u8
            }
            Row::Map => {
                let maps = assets::maps();
                let i = maps.iter().position(|m| *m == self.map).unwrap_or(0) as i32;
                let n = maps.len().max(1) as i32;
                if let Some(m) =
                    maps.get((i + if step == 0 { 1 } else { step }).rem_euclid(n) as usize)
                {
                    self.map = m.clone();
                }
            }
            Row::Camera => {
                let n = CAMERAS.len() as i32;
                self.camera =
                    (self.camera as i32 + if step == 0 { 1 } else { step }).rem_euclid(n) as usize;
            }
            Row::Ghost => self.ghost = !self.ghost,
            Row::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                set_fullscreen(self.fullscreen);
                if !self.fullscreen {
                    let (w, h) = RESOLUTIONS[self.resolution];
                    request_new_screen_size(w, h);
                }
            }
            Row::Resolution => {
                let n = RESOLUTIONS.len() as i32;
                self.resolution = (self.resolution as i32 + if step == 0 { 1 } else { step })
                    .rem_euclid(n) as usize;
                if !self.fullscreen {
                    let (w, h) = RESOLUTIONS[self.resolution];
                    request_new_screen_size(w, h);
                }
            }
            Row::Resume | Row::Quit => return false,
        }
        true
    }
}

/// COL.PCX in the top-right corner with the gates (next one yellow, start white), the ghost and the car.
fn draw_minimap(
    colour: &Texture2D,
    zones: &[(usize, usize, Zone)],
    track: Option<&Track>,
    next: usize,
    car: &SimCar,
    ghost: Option<[f32; 2]>,
    top: f32,
) {
    let size = (screen_width().min(screen_height()) * 0.28).round();
    let (x0, y0) = (screen_width() - size - 12.0, top + 12.0);
    let to_map = |[x, y]: [f32; 2]| vec2(x0 + x / 65536.0 * size, y0 + y / 65536.0 * size);
    draw_rectangle(
        x0 - 3.0,
        y0 - 3.0,
        size + 6.0,
        size + 6.0,
        Color::new(0.0, 0.0, 0.0, 0.6),
    );
    let params = DrawTextureParams {
        dest_size: Some(vec2(size, size)),
        ..Default::default()
    };
    draw_texture_ex(colour, x0, y0, Color::new(1.0, 1.0, 1.0, 0.85), params);
    let cell = size / SIZE as f32;
    for &(x, y, z) in zones {
        let c = match z {
            Zone::Reset => Color::new(1.0, 0.1, 0.1, 0.45),
            Zone::Boost(_) => Color::new(0.2, 0.9, 1.0, 0.8),
            Zone::Slow(_) => Color::new(0.45, 0.28, 0.1, 0.8),
        };
        draw_rectangle(x0 + x as f32 * cell, y0 + y as f32 * cell, cell, cell, c);
    }
    if let Some(t) = track {
        for (g, ends) in t.ends.iter().enumerate() {
            let (a, b) = (to_map(ends[0]), to_map(ends[1]));
            let (w, c) = match g {
                _ if g == next => (4.0, YELLOW),
                0 => (2.5, WHITE),
                _ => (2.0, Color::new(1.0, 1.0, 1.0, 0.5)),
            };
            draw_line(a.x, a.y, b.x, b.y, w, c);
        }
    }
    if let Some(g) = ghost {
        let g = to_map(g);
        draw_circle(g.x, g.y, 4.0, Color::new(1.0, 1.0, 1.0, 0.6));
    }
    let [x, y, _] = car.centre();
    let p = to_map([x, y]);
    let (sin, cos) = heading(car).sin_cos();
    let nose = p + vec2(sin, -cos) * 9.0;
    draw_line(p.x, p.y, nose.x, nose.y, 2.0, RED);
    draw_circle(p.x, p.y, 4.0, RED);
}

fn draw_overlay(gfx: &Graphics, selected: usize) {
    draw_rectangle(
        0.0,
        0.0,
        screen_width(),
        screen_height(),
        Color::new(0.0, 0.0, 0.0, 0.55),
    );
    let (x, mut y) = (screen_width() / 2.0 - 220.0, screen_height() / 2.0 - 150.0);
    draw_text("PAUSED", x, y, 44.0, WHITE);
    y += 50.0;
    for (i, &row) in ROWS.iter().enumerate() {
        if row == Row::Filter {
            draw_text("Graphics", x, y, 24.0, GRAY);
            y += 32.0;
        }
        if row == Row::Quit {
            y += 12.0;
        }
        let colour = if i == selected { YELLOW } else { WHITE };
        draw_text(
            &format!(
                "{} {}",
                if i == selected { ">" } else { " " },
                gfx.label(row)
            ),
            x,
            y,
            30.0,
            colour,
        );
        y += 36.0;
    }
    draw_text(
        "up/down select   enter/left/right change   esc resume",
        x,
        y + 16.0,
        18.0,
        GRAY,
    );
}

const CRT_VERTEX: &str = "#version 100
attribute vec3 position;
attribute vec2 texcoord;
attribute vec4 color0;
varying lowp vec2 uv;
uniform mat4 Model;
uniform mat4 Projection;
void main() {
    gl_Position = Projection * Model * vec4(position, 1);
    uv = texcoord;
}
";

/// Curvature, scanlines, aperture-grille RGB mask and vignette.
const CRT_FRAGMENT: &str = "#version 100
precision mediump float;
varying lowp vec2 uv;
uniform sampler2D Texture;
uniform vec2 Resolution;
void main() {
    vec2 c = uv * 2.0 - 1.0;
    c *= 1.0 + 0.045 * dot(c.yx, c.yx);          // barrel
    vec2 p = c * 0.5 + 0.5;
    if (p.x < 0.0 || p.x > 1.0 || p.y < 0.0 || p.y > 1.0) { gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0); return; }
    vec3 col = texture2D(Texture, p).rgb;
    float line = 0.72 + 0.28 * sin(p.y * Resolution.y * 3.14159 / 1.5);   // a dark line every 3 px
    float m = mod(gl_FragCoord.x, 3.0);
    vec3 mask = m < 1.0 ? vec3(1.0, 0.8, 0.8) : (m < 2.0 ? vec3(0.8, 1.0, 0.8) : vec3(0.8, 0.8, 1.0));
    float vig = clamp(pow(16.0 * p.x * p.y * (1.0 - p.x) * (1.0 - p.y), 0.25), 0.0, 1.0);
    gl_FragColor = vec4(col * line * mask * vig * 1.35, 1.0);
}
";

fn window_conf() -> macroquad::conf::Conf {
    macroquad::conf::Conf {
        miniquad_conf: Conf {
            window_title: "Reformers".into(),
            window_width: 1280,
            window_height: 800,
            high_dpi: true,
            ..Default::default()
        },
        update_on: None, // redraw every frame, not only on input events
        draw_call_vertex_capacity: 10_000, // one terrain chunk per draw call
        draw_call_index_capacity: 50_000,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let car_n = args
        .iter()
        .position(|a| a == "--car")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or("1".into());
    let read = |f: &str| assets::read(f).unwrap_or_else(|e| panic!("{e}"));
    let saved = settings_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let mut map = pick_map(&args, &saved);
    let (terrain, mut level) = load_level(&map).unwrap_or_else(|e| panic!("{e}"));
    let mut sim = Sim::new(terrain);
    if let Some(t) = &level.track {
        sim.set_soft_dirt(t.soft_dirt.clone());
    }
    let mut stamps = watched(&map);
    let mut next_poll = 0.0;
    // All five original cars, switchable with 1–5. Only CAR1/CAR2 ship a skin; the rest use TEXTURES.PCX like 0x1323.
    // cars are TEREP2's: always its palette (MAPTEX.PCX carries it), whatever map they drive on
    let car_palette = pcx::decode(&read("MAPTEX.PCX"))
        .expect("MAPTEX.PCX")
        .palette;
    let cars: Vec<(Car, Skin)> = (1..=5)
        .map(|n| {
            let model = car::parse(&read(&format!("CAR{n}.DAT"))).expect("car");
            let skin =
                assets::read(&format!("CAR{n}.PCX")).unwrap_or_else(|_| read("TEXTURES.PCX"));
            (model, skin_texture(&skin, &car_palette))
        })
        .collect();
    let mut current = car_n.parse::<usize>().unwrap_or(1).clamp(1, 5) - 1;
    let mut roll = [0.0f32; 4];
    let mut wireframe = false;
    let mut speed = 0.0f32;
    sim.dig_everywhere = args.iter().any(|a| a == "--soft");
    let (spawn, spawn_heading) = level
        .track
        .as_ref()
        .and_then(|t| t.spawn.as_ref())
        .map_or((SPAWN, 0.0), |s| ((s.x, s.y), s.heading));
    sim.cars.push(SimCar::spawn_facing(
        &cars[current].0,
        &sim.terrain,
        spawn.0,
        spawn.1,
        spawn_heading,
    ));
    let mut race = Race::new(best_key(&level, current).and_then(|k| load_best(&k)));
    let mut ghost = load_ghost(&level, current);
    let mut recording = Ghost::default(); // this lap so far
    let mut ticks = 0u64;
    let mut reset_zone = false; // drove into a reset zone: respawn like R next frame
    let mut banner: Option<(String, f64)> = None; // message, shown until
    let mut last_centre = sim.cars[0].centre();

    let chunks = SIZE.div_ceil(CHUNK);
    let mut meshes: Vec<Mesh> = (0..chunks).map(|c| chunk_mesh(&sim.terrain, c)).collect();
    let colour_tex = Texture2D::from_image(&colour_image(&sim.terrain));
    let mut maptex = tiles(&level, &sim.terrain.palette);
    let terrain_material = fog_material(
        TERRAIN_VERTEX,
        TERRAIN_FRAGMENT,
        vec!["ColIdx".into(), "MapTex".into()],
    );
    let fog = fog_material(TERRAIN_VERTEX, FOG_FRAGMENT, vec![]);
    terrain_material.set_texture("MapTex", maptex.clone());
    let index_tex = Texture2D::from_image(&index_image(&sim.terrain));
    index_tex.set_filter(FilterMode::Nearest);
    terrain_material.set_texture("ColIdx", index_tex.clone());
    let mut shown_colour = sim.terrain.colour.clone();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let mut gfx = Graphics {
        map: map.clone(),
        camera: 0,
        nearest: true,
        textured: true,
        crt: false,
        minimap: true,
        ghost: false,
        fog: true,
        volume: 7,
        fullscreen: false,
        resolution: 1,
    };
    gfx.apply_text(&saved);
    gfx.map = map.clone(); // pick_map already weighed --map and the saved one
    // launch flags win over the saved settings
    gfx.nearest &= !flag("--smooth");
    gfx.textured &= !flag("--untextured");
    gfx.crt |= flag("--crt");
    gfx.minimap &= !flag("--no-minimap");
    gfx.ghost |= flag("--ghost");
    gfx.fog &= !flag("--no-fog");
    if flag("--mute") {
        gfx.volume = 0;
    }
    let audio = Audio::load().await;
    let (mut impacts, mut last_thud, mut skid) = (0u32, 0.0f64, 0.0f32);
    if gfx.fullscreen {
        set_fullscreen(true);
    } else if gfx.resolution != 1 {
        let (w, h) = RESOLUTIONS[gfx.resolution];
        request_new_screen_size(w, h);
    }
    let mut orbit = 0.0f32; // orbit camera angle around the car
    let crt = load_material(
        ShaderSource::Glsl {
            vertex: CRT_VERTEX,
            fragment: CRT_FRAGMENT,
        },
        MaterialParams {
            uniforms: vec![UniformDesc::new("Resolution", UniformType::Float2)],
            ..Default::default()
        },
    )
    .expect("CRT shader");
    let mut scene_target: Option<RenderTarget> = None;
    let mut menu_open = args.iter().any(|a| a == "--menu");
    let mut selected = 0usize;
    let mut shown = sim.terrain.height.clone();
    let mut original = shown.clone();
    let mut cam_pos = v(sim.cars[0].centre()) + vec3(0.0, 2.0, 4.0);
    let mut arm = 1.0f32; // share of the full chase-camera arm that's clear of the ground
    let mut acc = 0.0;
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let started = get_time();

    loop {
        // hot reload: edit a map image, save, and it's here within a second
        if get_time() >= next_poll || gfx.map != map {
            next_poll = get_time() + 1.0;
            let now = watched(&gfx.map);
            let switched = gfx.map != map;
            if now != stamps || switched {
                stamps = now;
                match load_level(&gfx.map) {
                    Ok((terrain, new)) => {
                        sim.set_terrain(terrain);
                        level = new;
                        if let Some(t) = &level.track {
                            sim.set_soft_dirt(t.soft_dirt.clone());
                        }
                        map = gfx.map.clone();
                        meshes = (0..chunks).map(|c| chunk_mesh(&sim.terrain, c)).collect();
                        colour_tex.update(&colour_image(&sim.terrain));
                        index_tex.update(&index_image(&sim.terrain));
                        maptex = tiles(&level, &sim.terrain.palette);
                        terrain_material.set_texture("MapTex", maptex.clone());
                        shown.copy_from_slice(&sim.terrain.height);
                        original.copy_from_slice(&sim.terrain.height);
                        shown_colour.copy_from_slice(&sim.terrain.colour);
                        // new map: start at its spawn; edited map: stay, unless buried by the new heights
                        let [x, y, z] = sim.cars[0].centre();
                        if switched {
                            let ((sx, sy), h) = level
                                .track
                                .as_ref()
                                .and_then(|t| t.spawn.as_ref())
                                .map_or((SPAWN, 0.0), |s| ((s.x, s.y), s.heading));
                            sim.cars[0] =
                                SimCar::spawn_facing(&cars[current].0, &sim.terrain, sx, sy, h);
                            roll = [0.0; 4];
                        } else if z < sim.terrain.height_at(x, y) {
                            let h = heading(&sim.cars[0]);
                            sim.cars[0] = SimCar::spawn_facing(
                                &cars[current].0,
                                &sim.terrain,
                                x as u16,
                                y as u16,
                                h,
                            );
                            roll = [0.0; 4];
                        }
                        // a changed map is a different track: new time trial, its own best lap
                        race = Race::new(best_key(&level, current).and_then(|k| load_best(&k)));
                        ghost = load_ghost(&level, current);
                        recording = Ghost::default();
                        let msg = if switched {
                            format!("map: {map}")
                        } else {
                            "map reloaded".into()
                        };
                        banner = Some((msg, get_time() + 1.5));
                    }
                    Err(e) => {
                        gfx.map = map.clone(); // a map that won't load: stay on this one
                        banner = Some((format!("reload failed: {e}"), get_time() + 5.0))
                    }
                }
            }
        }
        if is_key_pressed(KeyCode::Escape) {
            menu_open = !menu_open;
            selected = 0;
        }
        if menu_open {
            if is_key_pressed(KeyCode::Up) {
                selected = (selected + ROWS.len() - 1) % ROWS.len();
            }
            if is_key_pressed(KeyCode::Down) {
                selected = (selected + 1) % ROWS.len();
            }
            let step = if is_key_pressed(KeyCode::Left) {
                -1
            } else if is_key_pressed(KeyCode::Right) {
                1
            } else {
                0
            };
            let enter = is_key_pressed(KeyCode::Enter)
                || is_key_pressed(KeyCode::KpEnter)
                || is_key_pressed(KeyCode::Space);
            if step != 0 || enter {
                match ROWS[selected] {
                    Row::Resume if enter => menu_open = false,
                    Row::Quit if enter => break,
                    row => {
                        if gfx.change(row, step) {
                            save_settings(&gfx);
                        }
                    }
                }
            }
        }
        let filter = if gfx.nearest {
            FilterMode::Nearest
        } else {
            FilterMode::Linear
        };
        colour_tex.set_filter(filter);
        cars.iter()
            .for_each(|(_, skin)| skin.texture.set_filter(filter));
        maptex.set_filter(filter);
        if menu_open {
            // paused: no sim ticks, no time banked for later
            acc = 0.0;
        } else if is_key_pressed(KeyCode::W) {
            wireframe = !wireframe;
        } else if is_key_pressed(KeyCode::C) {
            gfx.camera = (gfx.camera + 1) % CAMERAS.len();
            save_settings(&gfx);
        }
        let pick = if menu_open {
            None
        } else {
            [
                KeyCode::Key1,
                KeyCode::Key2,
                KeyCode::Key3,
                KeyCode::Key4,
                KeyCode::Key5,
            ]
            .iter()
            .position(|&k| is_key_pressed(k))
        };
        if let Some(n) = pick {
            // best laps are per car: a new car starts a new time trial
            current = n;
            race = Race::new(best_key(&level, current).and_then(|k| load_best(&k)));
            ghost = load_ghost(&level, current);
            recording = Ghost::default();
        }
        if !menu_open && is_key_pressed(KeyCode::F) {
            // repair: a brand-new car where this one stands
            let [x, y, _] = sim.cars[0].centre();
            let h = heading(&sim.cars[0]);
            sim.cars[0] =
                SimCar::spawn_facing(&cars[current].0, &sim.terrain, x as u16, y as u16, h);
            roll = [0.0; 4];
            banner = Some(("repaired".into(), get_time() + 1.0));
        }
        if (!menu_open && is_key_pressed(KeyCode::R)) || pick.is_some() || reset_zone {
            reset_zone = false;
            // time trial: back to the last checkpoint, facing the next; else upright where the car is
            let [x, y, _] = sim.cars[0].centre();
            let ([x, y], h) = level
                .track
                .as_ref()
                .filter(|_| pick.is_none())
                .and_then(|t| race.respawn(t))
                .unwrap_or(([x, y], heading(&sim.cars[0])));
            // R keeps the dents (F repairs); a newly picked car comes fresh
            sim.cars[0] = if pick.is_some() {
                SimCar::spawn_facing(&cars[current].0, &sim.terrain, x as u16, y as u16, h)
            } else {
                sim.cars[0].respawned(&cars[current].0, &sim.terrain, x as u16, y as u16, h)
            };
            if let Some(t) = &level.track {
                race.place(t, [x, y]);
            }
            roll = [0.0; 4];
        }
        let (model, skin) = (&cars[current].0, &cars[current].1);
        let keys = Keys {
            throttle: is_key_down(KeyCode::Up) || shot.is_some(),
            reverse: is_key_down(KeyCode::Down),
            left: is_key_down(KeyCode::Left) || (shot.is_some() && get_time() - started > 2.0),
            right: is_key_down(KeyCode::Right),
            brake: is_key_down(KeyCode::Space) || is_key_down(KeyCode::Kp5),
        };
        // Fixed 120 Hz sim like the original timer interrupt; drawing runs free.
        if !menu_open {
            acc += get_frame_time().min(0.25);
        }
        while acc >= 1.0 / TICK_HZ as f32 {
            sim.tick(&[keys]);
            acc -= 1.0 / TICK_HZ as f32;
            ticks += 1;
            if let Some(t) = &level.track {
                reset_zone |= t.apply_zone(&mut sim.cars[0]);
            }
            if let Some(t) = &level.track {
                let [x, y, _] = sim.cars[0].centre();
                let n = t.gates.len();
                let ev = race.update(t, [x, y], ticks);
                if let Some(a) = &audio {
                    a.chime(&ev, gfx.volume as f32 / 10.0);
                }
                let msg = match ev {
                    Some(Event::Start) => {
                        recording = Ghost::default();
                        Some("GO!".to_string())
                    }
                    Some(Event::Gate(g)) => Some(match race.split_delta() {
                        Some(d) => format!(
                            "checkpoint {g}/{}   {}{}",
                            n - 1,
                            if d < 0 { "-" } else { "+" },
                            format_time(d.unsigned_abs())
                        ),
                        None => format!("checkpoint {g}/{}", n - 1),
                    }),
                    Some(Event::Lap { time, best }) => {
                        let lap = std::mem::take(&mut recording);
                        if best && let (Some(k), Some(b)) = (best_key(&level, current), &race.best)
                        {
                            save_best(&k, b);
                            save_ghost(&k, &lap);
                            ghost = lap;
                        }
                        Some(format!(
                            "lap {}{}",
                            format_time(time),
                            if best { "   best!" } else { "" }
                        ))
                    }
                    None => None,
                };
                if let Some(m) = msg {
                    banner = Some((m, get_time() + 2.5));
                }
            }
            let spin = sim.cars[0].spin();
            for i in 0..4 {
                // spin is rim travel per tick (16.16 world units)
                roll[i] += spin[i] as f32 / 65536.0 / model.atoms[i].radius;
            }
            if race.started && race.lap_time(ticks).is_multiple_of(ghost::STEP) {
                recording.frames.push(Frame {
                    atoms: sim.cars[0].atom_positions(),
                    steer: sim.cars[0].steer_angle(),
                    roll,
                });
            }
        }
        if let Some(a) = &audio {
            let vol = if menu_open {
                0.0
            } else {
                gfx.volume as f32 / 10.0
            };
            let rim = sim.cars[0]
                .spin()
                .iter()
                .map(|s| (*s as f32 / 65536.0).abs())
                .sum::<f32>()
                / 4.0;
            skid += ((sim.particle_positions().count() as f32 / 40.0).min(1.0) - skid) * 0.2;
            a.update(0.75 + rim * 0.15, keys.throttle, skid, vol);
            if sim.impacts > impacts && get_time() - last_thud > 0.15 && vol > 0.0 {
                play_sound(
                    &a.thud,
                    PlaySoundParams {
                        looped: false,
                        volume: vol * 0.9,
                    },
                );
                last_thud = get_time();
            }
        }
        impacts = sim.impacts;
        let c = sim.cars[0].centre();
        let moved = ((c[0] - last_centre[0]).powi(2) + (c[1] - last_centre[1]).powi(2)).sqrt();
        if !menu_open {
            speed += (moved / get_frame_time().max(1e-3) - speed) * 0.1;
        }
        last_centre = c;

        // Rebuild meshes whose rows (or neighbours, for shading) were dug.
        let mut dirty = vec![false; chunks];
        for y in 0..SIZE {
            if shown[y * SIZE..(y + 1) * SIZE] != sim.terrain.height[y * SIZE..(y + 1) * SIZE] {
                for r in y.saturating_sub(1)..=(y + 1).min(SIZE - 1) {
                    dirty[r / CHUNK] = true;
                    if r % CHUNK == 0 && r > 0 {
                        dirty[r / CHUNK - 1] = true; // shared edge row
                    }
                }
            }
        }
        for c in (0..chunks).filter(|&c| dirty[c]) {
            meshes[c] = chunk_mesh(&sim.terrain, c);
        }
        shown.copy_from_slice(&sim.terrain.height);
        if shown_colour != sim.terrain.colour {
            // landing dirt darkens COL cells
            colour_tex.update(&colour_image(&sim.terrain));
            index_tex.update(&index_image(&sim.terrain));
            shown_colour.copy_from_slice(&sim.terrain.colour);
        }

        // Chase camera behind the car (car-local +y is the rear).
        let atoms = sim.cars[0].atom_positions();
        let centre = v(sim.cars[0].centre());
        let back = (v(atoms[4]) - v(atoms[6]) + v(atoms[5]) - v(atoms[7])) * vec3(1.0, 0.0, 1.0);
        // Spring arm: shorten it where the ground is in the way (pull in at once, ease back out),
        // and never let the camera sink below the ground under it.
        let ground = |p: Vec3| sim.terrain.height_at(p.x * S, p.z * S) / S;
        let pivot = centre + vec3(0.0, 0.5, 0.0);
        let arm_dir = back.normalize_or_zero() * 3.0 + vec3(0.0, 0.8, 0.0);
        let clear = (1..=20)
            .map(|i| i as f32 / 20.0)
            .find(|&f| {
                let p = pivot + arm_dir * f;
                p.y < ground(p) + CAM_CLEARANCE
            })
            .map_or(1.0, |hit| (hit - 0.1).max(0.15));
        let dt = get_frame_time();
        arm = if clear < arm {
            clear
        } else {
            arm + (clear - arm) * (dt * 1.5).min(1.0)
        };
        cam_pos = cam_pos.lerp(pivot + arm_dir * arm, (dt * 6.0).min(1.0));
        cam_pos.y = cam_pos.y.max(ground(cam_pos) + CAM_CLEARANCE);

        // With CRT on, the 3D scene goes to an offscreen target (with depth) sized to the window.
        let (sw, sh) = (screen_width() as u32, screen_height() as u32);
        if gfx.crt
            && scene_target
                .as_ref()
                .is_none_or(|t| (t.texture.width() as u32, t.texture.height() as u32) != (sw, sh))
        {
            scene_target = Some(render_target_ex(
                sw,
                sh,
                RenderTargetParams {
                    sample_count: 1,
                    depth: true,
                },
            ));
        }
        let target = if gfx.crt { scene_target.clone() } else { None };
        let fwd = -back.normalize_or_zero();
        orbit +=
            (is_key_down(KeyCode::E) as i32 - is_key_down(KeyCode::Q) as i32) as f32 * dt * 2.0;
        let (position, look_at, up) = match gfx.camera {
            // bumper: low on the bonnet, looking down the road
            1 => {
                let p = centre + fwd * 0.25 + vec3(0.0, 0.45, 0.0);
                (p, p + fwd * 4.0 - vec3(0.0, 0.25, 0.0), Vec3::Y)
            }
            2 => {
                let (sin, cos) = orbit.sin_cos();
                let mut p = centre + vec3(sin * 4.0, 2.0, cos * 4.0);
                p.y = p.y.max(ground(p) + CAM_CLEARANCE);
                (p, centre + vec3(0.0, 0.3, 0.0), Vec3::Y)
            }
            // top-down: map-up is up on screen, like the minimap
            3 => (centre + vec3(0.0, 10.0, 0.001), centre, Vec3::NEG_Z),
            _ => (cam_pos, centre + vec3(0.0, 0.3, 0.0), Vec3::Y),
        };
        set_camera(&Camera3D {
            position,
            target: look_at,
            up,
            render_target: target.clone(),
            ..Default::default()
        });
        // fog off: push it past anything on screen
        let range = if gfx.fog { FOG_RANGE } else { [1e6, 1e6 + 1.0] };
        for m in [&fog, &terrain_material] {
            m.set_uniform("FogColor", FOG_COLOUR);
            m.set_uniform("FogRange", range);
        }
        clear_background(if gfx.fog {
            Color::new(FOG_COLOUR[0], FOG_COLOUR[1], FOG_COLOUR[2], 1.0)
        } else {
            Color::from_rgba(120, 160, 210, 255)
        });
        // untextured: plain COL colours (filtered per the option); textured: MAPTEX tiles per cell
        gl_use_material(if gfx.textured && level.maptex.is_some() {
            &terrain_material
        } else {
            &fog
        });
        for m in &mut meshes {
            m.texture = Some(colour_tex.clone());
            draw_mesh(m);
        }
        gl_use_material(&fog);
        let me = &sim.cars[0];
        // bumper camera sits inside the body: don't draw it
        let own = if gfx.camera == 1 { 0 } else { 3 };
        for m in car_meshes(
            model,
            &atoms,
            me.steer_angle(),
            &car_palette,
            &sim.terrain,
            skin,
            &roll,
            gfx.textured,
        )
        .iter()
        .take(own)
        {
            draw_mesh(m);
        }
        // the best lap's ghost, in step with the lap timer
        if gfx.ghost
            && race.started
            && let Some(f) = ghost.at(race.lap_time(ticks))
        {
            for mut m in car_meshes(
                model,
                &f.atoms,
                f.steer,
                &car_palette,
                &sim.terrain,
                skin,
                &f.roll,
                gfx.textured,
            ) {
                m.vertices
                    .iter_mut()
                    .for_each(|v| v.color[3] = (v.color[3] as u16 * 2 / 5) as u8);
                draw_mesh(&m);
            }
        }
        if wireframe {
            for (a, b, broken) in sim.cars[0].spring_links() {
                draw_line_3d(v(atoms[a]), v(atoms[b]), if broken { RED } else { WHITE });
            }
        }
        // pads: a coloured film on the ground (reset zones stay invisible: they're cliffs and water)
        for &(x, y, z) in &level.zone_cells {
            let colour = match z {
                Zone::Boost(_) => Color::new(0.2, 0.9, 1.0, 0.45),
                Zone::Slow(_) => Color::new(0.35, 0.22, 0.08, 0.6),
                Zone::Reset => continue,
            };
            let (wx, wy) = ((x as f32 + 0.5) * S, (y as f32 + 0.5) * S);
            let ground = sim.terrain.height_at(wx, wy);
            draw_cube(
                v([wx, wy, ground + 2.0]),
                vec3(1.0, 0.03, 1.0),
                None,
                colour,
            );
        }
        // gates: an arch (two posts and a banner) over each painted line
        if let Some(t) = &level.track {
            let mut top = vec![f32::MIN; t.gates.len()];
            for &(x, y, g) in &level.gate_cells {
                let ground = sim
                    .terrain
                    .height_at((x as f32 + 0.5) * S, (y as f32 + 0.5) * S);
                top[g] = top[g].max(ground);
            }
            // the bright next gate first: translucent ones after it, so they don't hide it
            let order =
                std::iter::once(race.next).chain((0..t.gates.len()).filter(|&g| g != race.next));
            for g in order {
                draw_gate(
                    &sim.terrain,
                    t.ends[g],
                    top[g] + GATE_HEIGHT * S,
                    g == 0,
                    g == race.next,
                    position,
                );
            }
        }
        for p in sim.particle_positions() {
            draw_cube(
                v(p),
                vec3(0.04, 0.04, 0.04),
                None,
                Color::from_rgba(90, 60, 30, 255),
            );
        }

        gl_use_default_material();
        set_default_camera();
        if let Some(t) = &target {
            crt.set_uniform("Resolution", vec2(screen_width(), screen_height()));
            gl_use_material(&crt);
            // render targets come out upside down
            let params = DrawTextureParams {
                dest_size: Some(vec2(screen_width(), screen_height())),
                flip_y: true,
                ..Default::default()
            };
            draw_texture_ex(&t.texture, 0.0, 0.0, WHITE, params);
            gl_use_default_material();
        }
        let dents = sim
            .terrain
            .height
            .iter()
            .zip(&original)
            .filter(|(a, b)| a != b)
            .count();
        let hud = format!(
            "arrows drive  space brake  R flip upright  F repair  1-5 car  C camera  esc menu   |   car {}  {:3.0} km/h   damage {:2.0}%   ruts {dents}{}",
            current + 1,
            speed * 0.019 * 3.6, // ~210 world units per 4 m car
            sim.cars[0].damage() * 100.0,
            if sim.dig_everywhere {
                "   (soft ground)"
            } else {
                ""
            }
        );
        let band = if level.track.is_some() { 64.0 } else { 32.0 };
        draw_rectangle(
            0.0,
            0.0,
            screen_width(),
            band,
            Color::new(0.0, 0.0, 0.0, 0.45),
        );
        draw_text(&hud, 12.0, 22.0, 18.0, WHITE);
        if let Some(t) = &level.track {
            let n = t.gates.len();
            let line = if race.started {
                let passed = (race.next + n - 1) % n;
                let opt = |l: Option<u64>| l.map_or("-:--.--".into(), format_time);
                format!(
                    "lap {}   {}   checkpoints {passed}/{}   last {}   best {}",
                    race.laps + 1,
                    format_time(race.lap_time(ticks)),
                    n - 1,
                    opt(race.last_lap),
                    opt(race.best.as_ref().map(|b| b.time))
                )
            } else {
                format!(
                    "time trial: drive through the start line   best {}",
                    race.best
                        .as_ref()
                        .map_or("-:--.--".into(), |b| format_time(b.time))
                )
            };
            draw_text(&line, 12.0, 52.0, 30.0, YELLOW);
        }
        if gfx.minimap {
            // ghost dot: midpoint of the axle atoms 4 and 6, like `SimCar::centre`
            let ghost_at = (gfx.ghost && race.started)
                .then(|| ghost.at(race.lap_time(ticks)))
                .flatten()
                .map(|f| {
                    [
                        (f.atoms[4][0] + f.atoms[6][0]) / 2.0,
                        (f.atoms[4][1] + f.atoms[6][1]) / 2.0,
                    ]
                });
            draw_minimap(
                &colour_tex,
                &level.zone_cells,
                level.track.as_ref(),
                race.next,
                &sim.cars[0],
                ghost_at,
                band,
            );
        }
        if let Some((msg, until)) = &banner
            && get_time() < *until
            && !menu_open
        {
            let size = measure_text(msg, None, 48, 1.0);
            draw_text(
                msg,
                (screen_width() - size.width) / 2.0,
                screen_height() * 0.2,
                48.0,
                YELLOW,
            );
        }
        if sim.cars[0].upside_down() && !menu_open {
            let msg = "upside down - press R";
            let size = measure_text(msg, None, 40, 1.0);
            draw_text(
                msg,
                (screen_width() - size.width) / 2.0,
                screen_height() * 0.3,
                40.0,
                YELLOW,
            );
        }
        if menu_open {
            draw_overlay(&gfx, selected);
        }
        if let Some(path) = &shot {
            if get_time() - started > 4.0 {
                get_screen_data().export_png(path);
                break;
            }
        }
        next_frame().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_maps() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_map(&args(&["x"]), ""), "terep2");
        assert_eq!(pick_map(&args(&["x"]), "camera=1\nmap=terep1\n"), "terep1");
        assert_eq!(
            pick_map(&args(&["x"]), "map=gone"),
            "terep2",
            "a deleted map falls back"
        );
        assert_eq!(
            pick_map(&args(&["x", "--map", "terep2-hills"]), "map=terep1"),
            "terep2-hills"
        );
        assert!(assets::maps().len() >= 3);
    }

    #[test]
    fn settings_round_trip() {
        let mut a = Graphics {
            map: "terep1".into(),
            camera: 2,
            nearest: false,
            textured: true,
            crt: true,
            minimap: false,
            ghost: true,
            fog: false,
            volume: 3,
            fullscreen: false,
            resolution: 3,
        };
        let mut b = Graphics {
            map: "x".into(),
            camera: 0,
            nearest: true,
            textured: false,
            crt: false,
            minimap: true,
            ghost: false,
            fog: true,
            volume: 10,
            fullscreen: true,
            resolution: 0,
        };
        b.apply_text(&a.to_text());
        assert_eq!(b.to_text(), a.to_text());
        a.apply_text("camera=99\nresolution=x\nbogus=true\nno equals sign");
        assert_eq!((a.camera, a.resolution), (2, 3), "bad values are skipped");
    }

    #[test]
    fn best_laps_round_trip() {
        let dir = std::env::temp_dir().join(format!("reformers-test-{}", std::process::id()));
        // SAFETY: the only test in this binary touching the environment.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        assert!(load_best("a-car1").is_none());
        save_best(
            "a-car1",
            &Lap {
                time: 900,
                splits: vec![100, 400],
            },
        );
        save_best(
            "b-car1",
            &Lap {
                time: 50,
                splits: vec![],
            },
        );
        save_best(
            "a-car1",
            &Lap {
                time: 800,
                splits: vec![90, 380],
            },
        ); // replaces, not appends
        let a = load_best("a-car1").unwrap();
        assert_eq!((a.time, a.splits), (800, vec![90, 380]));
        assert_eq!(load_best("b-car1").unwrap().time, 50);
        let text = std::fs::read_to_string(best_laps_file().unwrap()).unwrap();
        assert_eq!(text.lines().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
