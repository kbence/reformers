use reformers::sim::{Keys, Sim, SimCar, TICK_HZ};
use reformers::{assets, car, pcx, terrain::Terrain};
use std::process::exit;

/// `reform <in.pcx|in.bmp|in.png|in.pgm> <out.pgm|out.ppm|out.f32|out.rgb|out.pcx> [--map]`
/// .pgm = palette indices (for a MAP: the heights, as greyscale), .ppm = RGB image, .f32 = raw
/// LE floats (index / 255), .rgb = raw RGB, .pcx = 8-bit PCX (a greyscale height map becomes a
/// MAP.PCX for the DOS game: 256 wide gets the 320-wide layout, a blank legend strip on the right).
/// --map crops to 256×256.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "drive") {
        return drive(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "car-export") {
        return car_export(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "sounds") {
        return sounds(args.get(1).map_or("out", |s| s.as_str()));
    }
    let (input, output) = match &args[..] {
        [i, o] | [i, o, _] => (i, o),
        _ => {
            eprintln!("usage: reform <in.pcx|png|pgm|bmp> <out.pgm|ppm|f32|rgb|pcx> [--map]");
            exit(2);
        }
    };
    let data = std::fs::read(input).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
    let mut img = pcx::decode_image(&data).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
    if args.iter().any(|a| a == "--map") {
        img = img.crop(256, 256);
    }
    // ponytail: PGM/PPM need no image crate; add `png` when a viewer needs it.
    let out = match output.rsplit('.').next() {
        Some("f32") => img
            .pixels
            .iter()
            .flat_map(|&i| (i as f32 / 255.0).to_le_bytes())
            .collect(),
        Some("rgb") => img.to_rgb(),
        Some("pcx" | "PCX") => {
            if img.width == 256 {
                let pixels = img
                    .pixels
                    .chunks_exact(256)
                    .flat_map(|r| [r, &[0; 64]].concat());
                img.pixels = pixels.collect();
                img.width = 320;
            }
            pcx::encode(&img)
        }
        ext => {
            let (magic, body) = if ext == Some("ppm") {
                ("P6", img.to_rgb())
            } else {
                ("P5", img.pixels.clone())
            };
            let mut out = format!("{magic}\n{} {}\n255\n", img.width, img.height).into_bytes();
            out.extend(body);
            out
        }
    };
    std::fs::write(output, out).unwrap_or_else(|e| fail(&format!("{output}: {e}")));
}

/// `reform drive [--soft] [--car N] <script> <out-prefix>`
/// Script: space-separated `KEYS:SECONDS` steps; keys t(hrottle) r(everse) l(eft) (r)i(ght) b(rake), `-` = none.
/// Example: `t:3 tl:2 t:2 b:1`. Writes <prefix>.pgm (height map) and <prefix>_dents.pgm (changed cells).
/// --soft makes all ground diggable (the shipped COL.PCX has no diggable colour).
fn drive(args: &[String]) {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let car_n = args
        .iter()
        .position(|a| a == "--car")
        .and_then(|i| args.get(i + 1))
        .map_or("1", |s| s.as_str());
    let pos: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && !(i > &0 && args[i - 1] == "--car"))
        .map(|(_, a)| a)
        .collect();
    let [script, prefix] = pos[..] else {
        fail("usage: reform drive [--soft] [--car N] <script> <out-prefix>")
    };
    let read = |f: &str| assets::read(f).unwrap_or_else(|e| fail(&e));
    let terrain = Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX"))
        .unwrap_or_else(|e| fail(&e));
    let before = terrain.height.clone();
    let mut sim = Sim::new(terrain);
    sim.dig_everywhere = flag("--soft");
    if let Ok(cfg) = assets::read("sim.cfg") {
        if cfg.len() >= 4 {
            sim.cfg = (
                i16::from_le_bytes([cfg[0], cfg[1]]),
                i16::from_le_bytes([cfg[2], cfg[3]]),
            );
        }
    }
    let car = car::parse(&read(&format!("CAR{car_n}.DAT"))).unwrap_or_else(|e| fail(&e));
    sim.cars
        .push(SimCar::spawn(&car, &sim.terrain, 0x8000, 0x7a00));

    for step in script.split_whitespace() {
        let (keys, secs) = step
            .split_once(':')
            .unwrap_or_else(|| fail(&format!("bad step {step}")));
        let secs: f32 = secs
            .parse()
            .unwrap_or_else(|_| fail(&format!("bad seconds in {step}")));
        let k = Keys {
            throttle: keys.contains('t'),
            reverse: keys.contains('r'),
            left: keys.contains('l'),
            right: keys.contains('i'),
            brake: keys.contains('b'),
        };
        for _ in 0..(secs * TICK_HZ as f32) as u32 {
            sim.tick(&[k]);
        }
        let [x, y, z] = sim.cars[0].centre();
        let a = sim.cars[0].atom_positions();
        // heading: from the atom 4/5 midpoint to the atom 6/7 midpoint (car-local −y = front)
        let heading = (a[6][0] + a[7][0] - a[4][0] - a[5][0])
            .atan2(a[6][1] + a[7][1] - a[4][1] - a[5][1])
            .to_degrees();
        let up = a[8][2] - a[18][2]; // roof-ish minus floor-ish: negative means upside down
        println!(
            "{step:>8}  car at x={x:8.1} y={y:8.1} z={z:7.1}  heading {heading:6.1}°  roof-floor {up:5.1}  spin {:?}",
            sim.cars[0].spin().map(|s| s >> 16)
        );
    }

    let dents = sim
        .terrain
        .height
        .iter()
        .zip(&before)
        .filter(|(a, b)| a != b)
        .count();
    let broken = sim.cars[0].broken_springs();
    println!("dented cells: {dents}, broken springs: {broken}");
    let pgm = |px: &[u8]| [format!("P5\n256 256\n255\n").into_bytes(), px.to_vec()].concat();
    let mask: Vec<u8> = sim
        .terrain
        .height
        .iter()
        .zip(&before)
        .map(|(a, b)| if a != b { 255 } else { 0 })
        .collect();
    for (path, px) in [
        (format!("{prefix}.pgm"), &sim.terrain.height),
        (format!("{prefix}_dents.pgm"), &mask),
    ] {
        std::fs::write(&path, pgm(px)).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1);
}

/// `reform sounds [dir]`: writes the game's synthesised sounds as WAVs, to listen to them on their own.
fn sounds(dir: &str) {
    use reformers::sound;
    let all = [
        // the original's engine (F-number 0x30 + revs, block 4): idle, mid revs, flat out
        (
            "engine_original_idle",
            sound::engine_loop(sound::engine_note_hz(48)),
        ),
        (
            "engine_original_mid",
            sound::engine_loop(sound::engine_note_hz(150)),
        ),
        (
            "engine_original_fast",
            sound::engine_loop(sound::engine_note_hz(300)),
        ),
        ("thud", sound::thud()),
        (
            "thud_original_pitch",
            sound::render_patch(&sound::THUD_PATCH, sound::opl_hz(0x180, 0), 0.05, 0.6),
        ),
        ("skid", sound::skid_loop()),
        ("squeal", sound::squeal_loop()),
        ("checkpoint", sound::chime(&[(880.0, 0.3)])),
    ];
    // the recorded engine as the game pitches it for the same F-numbers, when installed
    let recorded = assets::read("sounds/engine/loop_0.wav")
        .ok()
        .and_then(|b| sound::read_wav(&b));
    let all: Vec<(&str, Vec<f32>)> = match &recorded {
        Some(idle) => all
            .into_iter()
            .chain([
                ("engine_recorded_idle", sound::speed_up(idle, 90.0 / 43.5)),
                (
                    "engine_recorded_mid",
                    sound::speed_up(idle, 90.0 * (150.0f32 / 48.0).powf(0.74) / 43.5),
                ),
                (
                    "engine_recorded_fast",
                    sound::speed_up(idle, 90.0 * (300.0f32 / 48.0).powf(0.74) / 43.5),
                ),
            ])
            .collect(),
        None => all.into_iter().collect(),
    };
    std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(&format!("{dir}: {e}")));
    for (name, samples) in all {
        let path = format!("{dir}/{name}.wav");
        std::fs::write(&path, sound::wav(&samples))
            .unwrap_or_else(|e| fail(&format!("{path}: {e}")));
        println!("{path}");
    }
}

/// World units per Blender unit (metre): a car is ~210 units, ~4 m.
const UNITS_PER_M: f32 = 52.0;

/// `reform car-export <CAR.DAT> <out.obj>` (#55): the car as Wavefront OBJ for Blender, plus
/// `out.mtl` and its skin as `out.png`. A name like `CAR1.DAT` is looked up in the assets.
///
/// Axes: OBJ is Y-up and right-handed, the car left-handed (x = right, y = back to front −y,
/// z = up), so x is mirrored; Blender's default OBJ import then shows the car upright, its
/// front towards Blender's −Y, 1 unit = 1 m. Atoms are the vertices; body polygons are faces
/// (flat ones get a palette-colour material, textured ones UVs on the skin; shadows are left
/// out); wheels and the driver are small cubes named `wheel_FL`… `driver`, sized by radius;
/// springs are line objects by type: `springs_frame` (rigid), `springs_suspension` (damped),
/// `springs_rope` (slack).
fn car_export(args: &[String]) {
    let [input, output] = args else {
        fail("usage: reform car-export <CAR.DAT> <out.obj>")
    };
    let read = |f: &str| {
        std::fs::read(f)
            .or_else(|_| assets::read(f))
            .unwrap_or_else(|e| fail(&format!("{f}: {e}")))
    };
    let model = car::parse(&read(input)).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
    let stem = output.strip_suffix(".obj").unwrap_or(output);
    let name = std::path::Path::new(stem)
        .file_name()
        .map_or("car".into(), |n| n.to_string_lossy().into_owned());
    let palette = pcx::decode(&assets::read("MAPTEX.PCX").unwrap_or_else(|e| fail(&e)))
        .unwrap_or_else(|e| fail(&e))
        .palette;
    // the skin next to the .DAT (CARn.PCX), else the shared TEXTURES.PCX, as the game does
    let skin_name = input
        .rsplit_once('.')
        .map_or(format!("{input}.PCX"), |(s, _)| format!("{s}.PCX"));
    let skin = pcx::decode(
        &std::fs::read(&skin_name)
            .or_else(|_| assets::read(&skin_name))
            .unwrap_or_else(|_| read("TEXTURES.PCX")),
    )
    .unwrap_or_else(|e| fail(&e));

    let at = |p: [f32; 3]| [-p[0] / UNITS_PER_M, p[2] / UNITS_PER_M, -p[1] / UNITS_PER_M];
    let n = model.atoms.len();
    let mut obj = format!(
        "# Reformers car {input}, from `reform car-export` (#55)\n\
         # drive: {:?}; {} atoms, {} springs; 1 unit = 1 m ({UNITS_PER_M} world units)\n\
         mtllib {name}.mtl\n",
        model.drive,
        n,
        model.springs.len()
    );
    // the atoms, 1-based: vertex i + 1
    for a in &model.atoms {
        let [x, y, z] = at(a.pos);
        obj += &format!("v {x:.5} {y:.5} {z:.5}\n");
    }
    // the body
    obj += "o body\n";
    let (w, h) = (skin.width as f32, skin.height as f32);
    let mut vt = 0;
    let mut material = String::new();
    for prim in &model.prims {
        let Some(mut face) = car::face_atoms(prim, n) else {
            continue; // shadows, wheels
        };
        // mirroring x reverses the winding: write the corners the other way round, so each
        // face's front (the side it's seen from) stays its front in Blender
        face.reverse();
        let use_mtl = match prim {
            car::Prim::Flat { colour, .. } => format!("c{colour}"),
            _ => "skin".into(),
        };
        if use_mtl != material {
            obj += &format!("usemtl {use_mtl}\n");
            material = use_mtl;
        }
        match prim {
            car::Prim::Textured { verts } => {
                // UVs per corner, in the order face_atoms kept them (closing vertex dropped)
                let mut uvs: Vec<[f32; 2]> = verts.iter().take(face.len()).map(|v| v.1).collect();
                uvs.reverse();
                let mut f = String::from("f");
                for (k, &a) in face.iter().enumerate() {
                    let [u, v] = uvs[k];
                    obj += &format!("vt {:.5} {:.5}\n", u / w, 1.0 - v / h);
                    vt += 1;
                    f += &format!(" {}/{vt}", a + 1);
                }
                obj += &(f + "\n");
            }
            _ => {
                let f: Vec<String> = face.iter().map(|a| (a + 1).to_string()).collect();
                obj += &format!("f {}\n", f.join(" "));
            }
        }
    }
    // wheels and the driver: cubes around the atom, half a side = its radius
    let mut base = n;
    let mut cube = |obj: &mut String, label: &str, a: &car::Atom| {
        let c = at(a.pos);
        let r = a.radius / UNITS_PER_M;
        *obj += &format!("o {label}\n");
        for k in 0..8 {
            let s = |bit: usize| if k >> bit & 1 == 1 { r } else { -r };
            *obj += &format!(
                "v {:.5} {:.5} {:.5}\n",
                c[0] + s(0),
                c[1] + s(1),
                c[2] + s(2)
            );
        }
        // outward-facing quads (corner bits: 0 = x, 1 = y, 2 = z)
        for q in [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ] {
            let f: Vec<String> = q.iter().map(|i| (base + i + 1).to_string()).collect();
            *obj += &format!("f {}\n", f.join(" "));
        }
        base += 8;
    };
    for a in &model.atoms {
        let label = match a.kind {
            car::AtomKind::FrontWheel | car::AtomKind::RearWheel => {
                let side = if a.pos[0] < 0.0 { "L" } else { "R" };
                let end = if a.kind == car::AtomKind::FrontWheel {
                    "F"
                } else {
                    "R"
                };
                format!("wheel_{end}{side}")
            }
            car::AtomKind::Driver => "driver".into(),
            car::AtomKind::Body => continue,
        };
        cube(&mut obj, &label, a);
    }
    // springs, as lines between the atoms
    for (group, kind) in [
        ("springs_frame", 0),
        ("springs_suspension", 1),
        ("springs_rope", 2),
    ] {
        let lines: Vec<&car::Spring> = model
            .springs
            .iter()
            .filter(|s| {
                kind == match s.kind {
                    car::SpringKind::Rigid => 0,
                    car::SpringKind::Damped(_) => 1,
                    car::SpringKind::Slack => 2,
                }
            })
            .collect();
        if lines.is_empty() {
            continue;
        }
        obj += &format!("o {group}\n");
        for s in lines {
            obj += &format!("l {} {}\n", s.a + 1, s.b + 1);
        }
    }

    // materials: the palette colours used, and the skin
    let mut mtl = String::from("# Reformers car materials: cN = car palette colour N\n");
    let mut used: Vec<u8> = model
        .prims
        .iter()
        .filter_map(|p| match p {
            car::Prim::Flat { colour, verts } if !verts.iter().all(|e| e % 2 == 1) => Some(*colour),
            _ => None,
        })
        .collect();
    used.sort();
    used.dedup();
    for c in used {
        let [r, g, b] = palette[c as usize].map(|v| v as f32 / 255.0);
        // colours f0–f3 are the original's see-through windows
        let glass = if c >= 0xf0 { "d 0.4\n" } else { "" };
        mtl += &format!("newmtl c{c}\nKd {r:.3} {g:.3} {b:.3}\n{glass}");
    }
    mtl += &format!("newmtl skin\nKd 1 1 1\nmap_Kd {name}.png\n");

    // the skin, its background (the most common colour) see-through as in the game
    let mut count = [0usize; 256];
    skin.pixels.iter().for_each(|&i| count[i as usize] += 1);
    let bg = (0..256).max_by_key(|&i| count[i]).unwrap() as u8;
    let bytes = skin
        .pixels
        .chunks_exact(skin.width)
        .rev() // export_png flips rows
        .flatten()
        .flat_map(|&i| {
            let [r, g, b] = palette[i as usize];
            [r, g, b, if i == bg { 0 } else { 255 }]
        })
        .collect();
    let write = |path: String, data: &[u8]| {
        std::fs::write(&path, data).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
    };
    write(format!("{stem}.obj"), obj.as_bytes());
    write(format!("{stem}.mtl"), mtl.as_bytes());
    macroquad::texture::Image {
        bytes,
        width: skin.width as u16,
        height: skin.height as u16,
    }
    .export_png(&format!("{stem}.png"));
    println!("wrote {stem}.obj, {stem}.mtl, {stem}.png");
}
