//! Reformers, the game: `cargo run --release [-- --soft] [--car N]`.
//! Arrows drive, Space brakes, R puts the car back on its wheels (on the last checkpoint in a time
//! trial), 1–5 switch car, W toggles the spring wireframe, Esc opens the options overlay (resume,
//! graphics, quit). With `CHECK.PCX` present the map is a time trial: the timer starts at the start line.
//! `--menu` starts with the overlay open; `--smooth` / `--untextured` / `--crt` preset the graphics options.
//! `--shot out.png`: hold throttle + left, save a frame after 4 s and exit (for checking without a human).

use macroquad::prelude::*;
use reformers::car::{self, Car, Prim};
use reformers::sim::{Keys, Sim, SimCar, TICK_HZ};
use reformers::terrain::{SIZE, Terrain};
use reformers::track::{Event, Lap, Race, Track, format_time};
use reformers::{assets, pcx};
use std::path::PathBuf;

const S: f32 = 256.0; // world units per render unit (one terrain cell)
const CHUNK: usize = 32; // rows per mesh: macroquad meshes index with u16
const SPAWN: (u16, u16) = (0x8000, 0x7a00); // without CHECK.PCX

/// Best laps, one `key time split…` line per map + car, in the user's data dir.
fn best_laps_file() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let base = env("XDG_DATA_HOME")
        .or_else(|| env("HOME").map(|h| h.join(".local/share")))
        .or_else(|| env("APPDATA"))?;
    Some(base.join("reformers").join("best_laps.txt"))
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
uniform mat4 Model;
uniform mat4 Projection;
void main() {
    gl_Position = Projection * Model * vec4(position, 1);
    uv = texcoord;
    shade = color0 / 255.0;
}
";

/// Textured terrain: the cell's COL index (alpha) selects a MAPTEX tile, tiled once per cell.
const TERRAIN_FRAGMENT: &str = "#version 100
precision mediump float;
varying lowp vec2 uv;
varying lowp vec4 shade;
uniform sampler2D ColIdx;  // COL indices in red, nearest
uniform sampler2D MapTex;
void main() {
    vec2 cell = uv * 256.0;
    float idx = floor(texture2D(ColIdx, (floor(cell) + 0.5) / 256.0).r * 255.0 + 0.5);
    vec2 tile = vec2(mod(idx, 16.0), floor(idx / 16.0));
    vec2 st = (tile * 16.0 + 0.5 + fract(cell) * 15.0) / 256.0;  // stay inside the tile
    gl_FragColor = vec4(texture2D(MapTex, st).rgb * shade.rgb, 1.0);
}
";

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
fn car_meshes(
    model: &Car,
    car: &SimCar,
    t: &Terrain,
    skin: &Skin,
    roll: &[f32; 4],
    use_skin: bool,
) -> [Mesh; 3] {
    let atoms = car.atom_positions();
    let n = atoms.len();
    let w = |p: [f32; 3]| Vec3::from(p);
    let unit = |a: Vec3| a.normalize_or_zero();
    let side = unit(w(atoms[7]) - w(atoms[6]));
    let fwd = unit(w(atoms[4]) - w(atoms[6]) + w(atoms[5]) - w(atoms[7]));
    let steered = fwd * car.steer_angle().sin() + side * car.steer_angle().cos(); // like 0x11f0
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
        let [r, g, b] = t.palette[i as usize];
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
                } else {
                    let [r, g, b] = t.palette[*colour as usize];
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

const RESOLUTIONS: [(f32, f32); 4] = [
    (960.0, 600.0),
    (1280.0, 800.0),
    (1600.0, 1000.0),
    (1920.0, 1200.0),
];

/// Graphics options: one field + one menu row each.
struct Graphics {
    nearest: bool,
    textured: bool,
    crt: bool,
    fullscreen: bool,
    resolution: usize, // index into RESOLUTIONS, windowed mode only
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Resume,
    Filter,
    Textures,
    Crt,
    Fullscreen,
    Resolution,
    Quit,
}
const ROWS: [Row; 7] = [
    Row::Resume,
    Row::Filter,
    Row::Textures,
    Row::Crt,
    Row::Fullscreen,
    Row::Resolution,
    Row::Quit,
];

impl Graphics {
    fn label(&self, row: Row) -> String {
        let on = |b: bool| if b { "on" } else { "off" };
        match row {
            Row::Resume => "Resume".into(),
            Row::Filter => format!(
                "Texture filter: {}",
                if self.nearest { "nearest" } else { "smooth" }
            ),
            Row::Textures => format!("Textures: {}", on(self.textured)),
            Row::Crt => format!("CRT filter: {}", on(self.crt)),
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
    let (map_pcx, col_pcx) = (read("MAP.PCX"), read("COL.PCX"));
    let mut sim = Sim::new(Terrain::load(&map_pcx, &col_pcx).expect("map"));
    let check_pcx = assets::read("CHECK.PCX").ok();
    let track = check_pcx
        .as_ref()
        .map(|b| Track::load(b).unwrap_or_else(|e| panic!("{e}")));
    let map_key = check_pcx
        .as_ref()
        .map(|c| track_hash(&[&map_pcx, &col_pcx, c]));
    let best_key = |car: usize| map_key.map(|k| format!("{k:016x}-car{}", car + 1));
    // All five original cars, switchable with 1–5. Only CAR1/CAR2 ship a skin; the rest use TEXTURES.PCX like 0x1323.
    let cars: Vec<(Car, Skin)> = (1..=5)
        .map(|n| {
            let model = car::parse(&read(&format!("CAR{n}.DAT"))).expect("car");
            let skin =
                assets::read(&format!("CAR{n}.PCX")).unwrap_or_else(|_| read("TEXTURES.PCX"));
            (model, skin_texture(&skin, &sim.terrain.palette))
        })
        .collect();
    let mut current = car_n.parse::<usize>().unwrap_or(1).clamp(1, 5) - 1;
    let mut roll = [0.0f32; 4];
    let mut wireframe = false;
    let mut speed = 0.0f32;
    sim.dig_everywhere = args.iter().any(|a| a == "--soft");
    let (spawn, spawn_heading) = track
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
    let mut race = Race::new(best_key(current).and_then(|k| load_best(&k)));
    let mut ticks = 0u64;
    let mut banner: Option<(String, f64)> = None; // message, shown until
    let gate_cells: Vec<(usize, usize, usize)> = track.as_ref().map_or(vec![], |t| {
        (0..SIZE * SIZE)
            .filter_map(|i| {
                t.gate_at(i % SIZE, i / SIZE)
                    .map(|g| (i % SIZE, i / SIZE, g))
            })
            .collect()
    });
    let mut last_centre = sim.cars[0].centre();

    let chunks = SIZE.div_ceil(CHUNK);
    let mut meshes: Vec<Mesh> = (0..chunks).map(|c| chunk_mesh(&sim.terrain, c)).collect();
    let colour_tex = Texture2D::from_image(&colour_image(&sim.terrain));
    let maptex = maptex_texture(&read("MAPTEX.PCX"), &sim.terrain.palette);
    let terrain_material = load_material(
        ShaderSource::Glsl {
            vertex: TERRAIN_VERTEX,
            fragment: TERRAIN_FRAGMENT,
        },
        MaterialParams {
            textures: vec!["ColIdx".into(), "MapTex".into()],
            pipeline_params: PipelineParams {
                depth_test: Comparison::LessOrEqual,
                depth_write: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("terrain shader");
    terrain_material.set_texture("MapTex", maptex.clone());
    let index_tex = Texture2D::from_image(&index_image(&sim.terrain));
    index_tex.set_filter(FilterMode::Nearest);
    terrain_material.set_texture("ColIdx", index_tex.clone());
    let mut shown_colour = sim.terrain.colour.clone();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let mut gfx = Graphics {
        nearest: !flag("--smooth"),
        textured: !flag("--untextured"),
        crt: flag("--crt"),
        fullscreen: false,
        resolution: 1,
    };
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
    let original = shown.clone();
    let mut cam_pos = v(sim.cars[0].centre()) + vec3(0.0, 2.0, 4.0);
    let mut acc = 0.0;
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let started = get_time();

    loop {
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
                        gfx.change(row, step);
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
            race = Race::new(best_key(current).and_then(|k| load_best(&k)));
        }
        if (!menu_open && is_key_pressed(KeyCode::R)) || pick.is_some() {
            // time trial: back to the last checkpoint, facing the next; else upright where the car is
            let [x, y, _] = sim.cars[0].centre();
            let ([x, y], h) = track
                .as_ref()
                .filter(|_| pick.is_none())
                .and_then(|t| race.respawn(t))
                .unwrap_or(([x, y], heading(&sim.cars[0])));
            sim.cars[0] =
                SimCar::spawn_facing(&cars[current].0, &sim.terrain, x as u16, y as u16, h);
            if let Some(t) = &track {
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
            if let Some(t) = &track {
                let [x, y, _] = sim.cars[0].centre();
                let n = t.gates.len();
                let msg = match race.update(t, [x, y], ticks) {
                    Some(Event::Start) => Some("GO!".to_string()),
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
                        if best && let (Some(k), Some(lap)) = (best_key(current), &race.best) {
                            save_best(&k, lap);
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
        }
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
        let want = centre + back.normalize_or_zero() * 3.0 + vec3(0.0, 1.3, 0.0);
        cam_pos = cam_pos.lerp(want, (get_frame_time() * 4.0).min(1.0));

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
        set_camera(&Camera3D {
            position: cam_pos,
            target: centre + vec3(0.0, 0.3, 0.0),
            up: Vec3::Y,
            render_target: target.clone(),
            ..Default::default()
        });
        clear_background(Color::from_rgba(120, 160, 210, 255));
        // untextured: plain COL colours (filtered per the option); textured: MAPTEX tiles per cell
        if gfx.textured {
            gl_use_material(&terrain_material);
        }
        for m in &mut meshes {
            m.texture = Some(colour_tex.clone());
            draw_mesh(m);
        }
        gl_use_default_material();
        for m in car_meshes(model, &sim.cars[0], &sim.terrain, skin, &roll, gfx.textured) {
            draw_mesh(&m);
        }
        if wireframe {
            for (a, b, broken) in sim.cars[0].spring_links() {
                draw_line_3d(v(atoms[a]), v(atoms[b]), if broken { RED } else { WHITE });
            }
        }
        // gates: a low wall on each painted cell, the next one bright; a pole marks the next gate
        for &(x, y, g) in &gate_cells {
            let (wx, wy) = ((x as f32 + 0.5) * S, (y as f32 + 0.5) * S);
            let colour = if g == race.next {
                Color::new(1.0, 0.85, 0.1, 0.5)
            } else {
                Color::new(1.0, 1.0, 1.0, 0.15)
            };
            let ground = sim.terrain.height_at(wx, wy);
            draw_cube(
                v([wx, wy, ground + 0.3 * S]),
                vec3(1.0, 0.6, 1.0),
                None,
                colour,
            );
        }
        if let Some(t) = &track {
            let [x, y] = t.gates[race.next];
            let ground = sim.terrain.height_at(x, y);
            let bob = (get_time() * 3.0).sin() as f32 * 0.2;
            draw_cube(
                v([x, y, ground + 2.0 * S]),
                vec3(0.12, 4.0, 0.12),
                None,
                YELLOW,
            );
            draw_cube(
                v([x, y, ground + (4.3 + bob) * S]),
                vec3(0.6, 0.6, 0.6),
                None,
                YELLOW,
            );
        }
        for p in sim.particle_positions() {
            draw_cube(
                v(p),
                vec3(0.04, 0.04, 0.04),
                None,
                Color::from_rgba(90, 60, 30, 255),
            );
        }

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
            "arrows drive  space brake  R flip upright  1-5 car  W wireframe  esc menu   |   car {}  {:3.0} km/h   dents {dents}   broken springs {}{}",
            current + 1,
            speed * 0.019 * 3.6, // ~210 world units per 4 m car
            sim.cars[0].broken_springs(),
            if sim.dig_everywhere {
                "   (soft ground)"
            } else {
                ""
            }
        );
        let band = if track.is_some() { 64.0 } else { 32.0 };
        draw_rectangle(
            0.0,
            0.0,
            screen_width(),
            band,
            Color::new(0.0, 0.0, 0.0, 0.45),
        );
        draw_text(&hud, 12.0, 22.0, 18.0, WHITE);
        if let Some(t) = &track {
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
        // roof atom (8) below a floor atom (18): upside down
        if atoms[8][2] < atoms[18][2] && !menu_open {
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
