use reformers::sim::{Keys, Sim, SimCar, TICK_HZ};
use reformers::{assets, car, pcx, terrain::Terrain};
use std::process::exit;

/// `tool <in.pcx|in.bmp|in.png|in.pgm> <out.pgm|out.ppm|out.f32|out.rgb|out.pcx> [--map]`
/// .pgm = palette indices (for a MAP: the heights, as greyscale), .ppm = RGB image, .f32 = raw
/// LE floats (index / 255), .rgb = raw RGB, .pcx = 8-bit PCX (a greyscale height map becomes a
/// MAP.PCX for the DOS game: 256 wide gets the 320-wide layout, a blank legend strip on the right).
/// --map crops to 256×256.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "drive") {
        return drive(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "sounds") {
        return sounds(args.get(1).map_or("out", |s| s.as_str()));
    }
    let (input, output) = match &args[..] {
        [i, o] | [i, o, _] => (i, o),
        _ => {
            eprintln!("usage: tool <in.pcx|png|pgm|bmp> <out.pgm|ppm|f32|rgb|pcx> [--map]");
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

/// `tool drive [--soft] [--car N] <script> <out-prefix>`
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
        fail("usage: tool drive [--soft] [--car N] <script> <out-prefix>")
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

/// `tool sounds [dir]`: writes the game's synthesised sounds as WAVs, to listen to them on their own.
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
