//! Reformers, the game: `cargo run --release [-- --soft] [--car N]`.
//! Arrows drive, Space brakes, R puts the car back on its wheels (on the last checkpoint in a time
//! trial), 1–5 switch car, W toggles the spring wireframe, C cycles the camera, Esc opens the
//! options overlay (resume, map, camera, graphics, quit). `--map NAME` picks a map folder. With `CHECK.PCX` present the map is a time trial: the timer starts at the start line.
//! `--menu` starts with the overlay open; `--smooth` / `--untextured` / `--crt` / `--baked-light` preset the graphics options.
//! `--shot out.png`: hold throttle + left, save a frame after 4 s and exit (for checking without a human).

#[cfg(not(target_arch = "wasm32"))]
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use macroquad::audio::{
    PlaySoundParams, Sound, load_sound_from_bytes, play_sound, set_sound_volume,
};
use macroquad::miniquad::{BlendFactor, BlendState, BlendValue, Equation};
use macroquad::prelude::*;
use reformers::assets::data_dir;
use reformers::car::{self, Car, Prim};
use reformers::ghost::{self, Frame, Ghost};
use reformers::run::{self, Replay};
use reformers::sim::{Keys, REPAIR_CAP, Sim, SimCar, TICK_HZ, Trunk};
use reformers::terrain::{SIZE, Terrain};
use reformers::track::{Event, Lap, Race, Track, Zone, format_time};
use reformers::{assets, lighting, net, objects, pcx, sound};
use std::path::PathBuf;

const S: f32 = 256.0; // world units per render unit (one terrain cell)
const CHUNK: usize = 32;
const CAM_CLEARANCE: f32 = 0.3; // render units the camera keeps above the ground // rows per mesh: macroquad meshes index with u16
const SPAWN: (u16, u16) = (0x8000, 0x7a00); // without CHECK.PCX

/// Firing frequency of the recorded engine idle loop (`assets/sounds/engine/loop_0.wav`), measured.
const RECORDED_HZ: f32 = 43.5;

/// Engine voices sound at F-numbers (the original's pitch unit, `sound::engine_fnum`) from idle
/// 0x30 up to this, in ~3 % steps; one voice per player.
const ENGINE_TOP_FNUM: f32 = 400.0;
/// Mix levels: engine (flat out) + full skid + a thud stay under full scale even at volume
/// 10/10 (worst case 0.96). The thud at 0.9 clipped the mix on hard landings, heard as
/// crackle; found with --record-audio.
const THUD_LEVEL: f32 = 0.4;
/// Sliding at full tilt (crunch and squeal share it); was 0.25, a bit much on long slides.
const SKID_LEVEL: f32 = 0.16;
/// Slide particles in the air for the skid at full volume.
const SKID_FULL: f32 = 60.0;
/// Engine level against the other sounds: the FM tone is nearly a square wave at full scale.
const ENGINE_LEVEL: f32 = 0.4;

/// One engine voice: looping tones at ascending F-numbers, of which only the one nearest the
/// wanted pitch sounds; a 60 ms crossfade when that changes (no two pitches beating together).
struct EngineVoice {
    loops: Vec<Sound>,
    /// The same tones' samples, for --record-audio.
    samples: Vec<Vec<f32>>,
    fnums: Vec<f32>,
    weights: Vec<f32>,
    gain: f32,
}

impl EngineVoice {
    async fn load(steps: usize, render: impl Fn(f32) -> Vec<f32>) -> Option<EngineVoice> {
        let lo = 48.0f32;
        let fnums: Vec<f32> = (0..steps)
            .map(|i| lo * (ENGINE_TOP_FNUM / lo).powf(i as f32 / (steps - 1) as f32))
            .collect();
        let (mut loops, mut samples) = (vec![], vec![]);
        for &f in &fnums {
            let tone = render(f);
            let snd = load_sound_from_bytes(&sound::wav(&tone)).await.ok()?;
            samples.push(tone);
            play_sound(
                &snd,
                PlaySoundParams {
                    looped: true,
                    volume: 0.0,
                },
            );
            loops.push(snd);
        }
        let weights = vec![0.0; steps];
        Some(EngineVoice {
            loops,
            samples,
            fnums,
            weights,
            gain: 0.0,
        })
    }

    /// Sounds the tone nearest `fnum` at `gain` (0 = silent).
    fn set(&mut self, fnum: f32, gain: f32, dt: f32) {
        let near = (0..self.fnums.len())
            .min_by(|&a, &b| {
                (self.fnums[a] / fnum)
                    .ln()
                    .abs()
                    .total_cmp(&(self.fnums[b] / fnum).ln().abs())
            })
            .unwrap();
        let k = (dt / 0.06).min(1.0);
        self.gain = gain;
        for (j, w) in self.weights.iter_mut().enumerate() {
            let before = *w;
            *w += ((j == near) as u8 as f32 - *w) * k;
            if *w < 1e-3 {
                *w = 0.0;
            }
            if *w > 0.0 || before > 0.0 {
                set_sound_volume(&self.loops[j], gain * *w);
            }
        }
    }
}

/// --record-audio: what the game asks the mixer to play, mixed again here from the same samples
/// and volumes (all loops started together at load, so sample `t` is the same point in each).
struct AudioRecording {
    path: String,
    out: Vec<f32>,
    /// Thuds still ringing: (samples, position, volume).
    thuds: Vec<(usize, f32)>,
    last_write: f64,
}

impl AudioRecording {
    fn frame(&mut self, audio: &Audio, which: usize, skid_vol: [f32; 2], dt: f32) {
        let n = (dt * sound::RATE as f32) as usize;
        let start = self.out.len();
        let voices = if which == 1 && !audio.recorded.is_empty() {
            &audio.recorded
        } else {
            &audio.original
        };
        for i in 0..n {
            let t = start + i;
            let mut v = 0.0;
            for voice in voices {
                for (j, &w) in voice.weights.iter().enumerate() {
                    if w > 0.0 {
                        let s = &voice.samples[j];
                        v += s[t % s.len()] * w * voice.gain;
                    }
                }
            }
            v += audio.skid_samples[t % audio.skid_samples.len()] * skid_vol[0];
            v += audio.squeal_samples[t % audio.squeal_samples.len()] * skid_vol[1];
            for (pos, vol) in &mut self.thuds {
                if let Some(x) = audio.thud_samples.get(*pos) {
                    v += x * *vol;
                }
                *pos += 1;
            }
            self.out.push(v);
        }
        self.thuds
            .retain(|(pos, _)| *pos < audio.thud_samples.len());
        if get_time() - self.last_write > 3.0 {
            self.save();
        }
    }

    fn save(&mut self) {
        self.last_write = get_time();
        if let Err(e) = std::fs::write(&self.path, sound::wav(&self.out)) {
            eprintln!("can't write {}: {e}", self.path);
        }
    }
}

/// Engine sounds to choose from (Esc menu → Engine sound).
const ENGINE_SOUNDS: [&str; 2] = ["original (AdLib)", "recorded (CC0)"];

/// The game's sounds, synthesised by `sound` at start-up (plus the recorded engine, if installed).
struct Audio {
    /// [player] voices: the original's AdLib engine, and the recorded one when installed.
    original: Vec<EngineVoice>,
    recorded: Vec<EngineVoice>,
    /// Sliding: crunch on soft ground, squeal on hard.
    skid: Sound,
    squeal: Sound,
    thud: Sound,
    skid_samples: Vec<f32>,
    squeal_samples: Vec<f32>,
    thud_samples: Vec<f32>,
    checkpoint: Sound,
    lap: Sound,
    best: Sound,
}

impl Audio {
    async fn load() -> Option<Audio> {
        async fn load(samples: Vec<f32>) -> Option<Sound> {
            load_sound_from_bytes(&sound::wav(&samples)).await.ok()
        }
        let mut original = vec![];
        for _player in 0..2 {
            original.push(
                EngineVoice::load(72, |f| sound::engine_loop(sound::engine_note_hz(f as u16)))
                    .await?,
            );
        }
        // the recorded idle loop (assets/sounds/engine/loop_0.wav, see SOURCES.md there), sped
        // up: at ~43 Hz as recorded it's below most speakers, so idle plays at 90 Hz, rising
        // with the F-number like the original but less steeply
        let mut recorded = vec![];
        if let Some(idle) = assets::read("sounds/engine/loop_0.wav")
            .ok()
            .and_then(|b| sound::read_wav(&b))
        {
            for _player in 0..2 {
                recorded.push(
                    EngineVoice::load(36, |f| {
                        let hz = 90.0 * (f / 48.0).powf(0.74);
                        sound::speed_up(&idle, hz / RECORDED_HZ)
                    })
                    .await?,
                );
            }
        }
        let audio = Audio {
            original,
            recorded,
            skid: load(sound::skid_loop()).await?,
            squeal: load(sound::squeal_loop()).await?,
            thud: load(sound::thud()).await?,
            skid_samples: sound::skid_loop(),
            squeal_samples: sound::squeal_loop(),
            thud_samples: sound::thud(),
            checkpoint: load(sound::chime(&[(880.0, 0.3)])).await?,
            lap: load(sound::chime(&[(660.0, 0.15), (990.0, 0.4)])).await?,
            best: load(sound::chime(&[(660.0, 0.12), (880.0, 0.12), (1320.0, 0.5)])).await?,
        };
        for s in [&audio.skid, &audio.squeal] {
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

    /// Player `p`'s engine through voice `which` (ENGINE_SOUNDS) at F-number `fnum` and `gain`;
    /// the other voice falls silent.
    fn engine(&mut self, p: usize, which: usize, fnum: f32, gain: f32, dt: f32) {
        let use_recorded = which == 1 && !self.recorded.is_empty();
        if let Some(v) = self.original.get_mut(p) {
            v.set(fnum, if use_recorded { 0.0 } else { gain }, dt);
        }
        if let Some(v) = self.recorded.get_mut(p) {
            v.set(fnum, if use_recorded { gain } else { 0.0 }, dt);
        }
    }

    /// Sliding, `[soft, hard]` ground, 0–1 each.
    fn skid(&self, skid: [f32; 2], vol: f32) {
        set_sound_volume(&self.skid, vol * SKID_LEVEL * skid[0]);
        set_sound_volume(&self.squeal, vol * SKID_LEVEL * skid[1]);
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

/// How far the left stick (either way) and the triggers must move to count.
#[cfg(not(target_arch = "wasm32"))]
const STICK_DEAD: f32 = 0.35;
#[cfg(not(target_arch = "wasm32"))]
const TRIGGER_DEAD: f32 = 0.3;

#[cfg(not(target_arch = "wasm32"))]
/// Keys the first gamepad holds this frame, as the keyboard keys they stand for: left stick or
/// d-pad steers, right trigger or A throttles, left trigger or X reverses, B brakes.
fn pad_down(stick_x: f32, rt: f32, lt: f32, held: impl Fn(Button) -> bool) -> Vec<KeyCode> {
    [
        (
            stick_x < -STICK_DEAD || held(Button::DPadLeft),
            KeyCode::Left,
        ),
        (
            stick_x > STICK_DEAD || held(Button::DPadRight),
            KeyCode::Right,
        ),
        (rt > TRIGGER_DEAD || held(Button::South), KeyCode::Up),
        (lt > TRIGGER_DEAD || held(Button::West), KeyCode::Down),
        (held(Button::East), KeyCode::Space),
        (held(Button::Select), KeyCode::F), // gradual repair is held
    ]
    .into_iter()
    .filter_map(|(on, k)| on.then_some(k))
    .collect()
}

#[cfg(not(target_arch = "wasm32"))]
/// A gamepad button pressed this frame, as the key it stands for (menu and actions).
fn pad_pressed(b: Button) -> Option<KeyCode> {
    Some(match b {
        Button::Start => KeyCode::Escape,
        Button::DPadUp => KeyCode::Up,
        Button::DPadDown => KeyCode::Down,
        Button::DPadLeft => KeyCode::Left,
        Button::DPadRight => KeyCode::Right,
        Button::South => KeyCode::Enter,
        Button::North => KeyCode::R,
        Button::Select => KeyCode::F,
        Button::LeftTrigger => KeyCode::T,
        Button::RightTrigger => KeyCode::C,
        _ => return None,
    })
}

#[cfg(not(target_arch = "wasm32"))]
/// Which of a pad's analog inputs (left stick x, right trigger, left trigger) have been seen at
/// rest since it joined; the others read as 0. HOTAS throttles and pedals can rest fully
/// deflected and report it as both triggers pressed the moment they connect, which joins them
/// and holds Up and Down for good (the arrows couldn't throttle or reverse).
#[derive(Default)]
struct Rested([bool; 3]);

#[cfg(not(target_arch = "wasm32"))]
impl Rested {
    fn filter(&mut self, inputs: [f32; 3]) -> [f32; 3] {
        let dead = [STICK_DEAD, TRIGGER_DEAD, TRIGGER_DEAD];
        std::array::from_fn(|i| {
            self.0[i] |= inputs[i].abs() <= dead[i];
            if self.0[i] { inputs[i] } else { 0.0 }
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Polls the first connected gamepad: (keys held, keys pressed this frame). Empty without one.
/// Only a controller whose button has been pressed takes part (`active`): Windows lists wheels,
/// pedals, virtual joysticks and some headsets as gamepads, and one resting with an axis
/// deflected would otherwise hold a key down for good (P1 couldn't steer with the arrows).
fn poll_pad(
    gilrs: &mut Option<Gilrs>,
    active: &mut Option<(GamepadId, Rested)>,
) -> (Vec<KeyCode>, Vec<KeyCode>) {
    let Some(g) = gilrs else {
        return (vec![], vec![]);
    };
    let mut pressed = vec![];
    while let Some(ev) = g.next_event() {
        match ev.event {
            EventType::ButtonPressed(b, _) => {
                if active.as_ref().is_none_or(|(id, _)| *id != ev.id) {
                    *active = Some((ev.id, Rested::default()));
                }
                pressed.extend(pad_pressed(b));
            }
            EventType::Disconnected if active.as_ref().is_some_and(|(id, _)| *id == ev.id) => {
                *active = None
            }
            _ => {}
        }
    }
    let Some((pad, rested)) = active
        .as_mut()
        .and_then(|(id, r)| Some((g.connected_gamepad(*id)?, r)))
    else {
        return (vec![], pressed);
    };
    let axis = |a| pad.axis_data(a).map_or(0.0, |d| d.value());
    let trigger = |b| pad.button_data(b).map_or(0.0, |d| d.value());
    let [stick_x, rt, lt] = rested.filter([
        axis(Axis::LeftStickX),
        trigger(Button::RightTrigger2),
        trigger(Button::LeftTrigger2),
    ]);
    let down = pad_down(stick_x, rt, lt, |b| pad.is_pressed(b));
    (down, pressed)
}

/// No gamepads on the web (see Cargo.toml).
#[cfg(target_arch = "wasm32")]
fn poll_pad(_: &mut Option<()>, _: &mut Option<()>) -> (Vec<KeyCode>, Vec<KeyCode>) {
    (vec![], vec![])
}

/// Camera for following car `i`: (position, look-at, up). Updates that player's spring-arm
/// chase position `cam` and arm share `arm` (see the chase case).
/// One car in the race (#54): `players[i]` drives `sim.cars[i]`. Player 1 is this machine's
/// (offline) or the host's; split screen adds player 2 on WASD; online, one per machine.
struct Player {
    car: usize,
    race: Race,
    /// Wheel spin, for drawing the wheels.
    roll: [f32; 4],
    /// The chase camera, and the share of its arm that's clear of the ground.
    cam: Vec3,
    arm: f32,
    /// R / F / T pressed, acting on the next tick (so a replay reproduces them exactly).
    reset: bool,
    repair: bool,
    turn: bool,
    /// Repair double-tapped (gradual repair): a full repair, sent as repair + turn over.
    full: bool,
    /// When repair was last pressed, to spot a double tap.
    tapped: f64,
}

/// Two presses of repair within this many seconds are a double tap (a full repair).
const DOUBLE_TAP: f64 = 0.35;

impl Player {
    /// Repair pressed this frame (`hit`): under gradual repair, the second of two quick ones
    /// asks for a full repair.
    fn tap(&mut self, hit: bool, gradual: bool) {
        if hit {
            let now = get_time();
            self.full |= gradual && now - self.tapped < DOUBLE_TAP;
            self.tapped = now;
        }
    }

    fn new(car: usize, race: Race, cam: Vec3) -> Player {
        Player {
            car,
            race,
            roll: [0.0; 4],
            cam,
            arm: 1.0,
            reset: false,
            repair: false,
            turn: false,
            full: false,
            tapped: f64::MIN,
        }
    }
}

fn follow_camera(
    sim: &Sim,
    i: usize,
    mode: usize,
    orbit: f32,
    cam: &mut Vec3,
    arm: &mut f32,
    dt: f32,
) -> (Vec3, Vec3, Vec3) {
    let atoms = sim.cars[i].atom_positions();
    let centre = v(sim.cars[i].centre());
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
    *arm = if clear < *arm {
        clear
    } else {
        *arm + (clear - *arm) * (dt * 1.5).min(1.0)
    };
    *cam = cam.lerp(pivot + arm_dir * *arm, (dt * 6.0).min(1.0));
    cam.y = cam.y.max(ground(*cam) + CAM_CLEARANCE);
    let fwd = -back.normalize_or_zero();
    match mode {
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
        _ => (*cam, centre + vec3(0.0, 0.3, 0.0), Vec3::Y),
    }
}

/// Car `i`'s place on the starting grid, all facing `heading`: two side by side (the second 1.5
/// cells to the right of `spawn`), each further row 2.5 cells back.
fn grid(spawn: (u16, u16), heading: u16, i: usize) -> (u16, u16) {
    let a = heading as f32 / 65536.0 * std::f32::consts::TAU;
    let (right, back) = ((i % 2) as f32 * 384.0, (i / 2) as f32 * 640.0);
    (
        (spawn.0 as f32 + a.cos() * right - a.sin() * back) as u16,
        (spawn.1 as f32 + a.sin() * right + a.cos() * back) as u16,
    )
}

/// The screen as a PNG in the data folder's `screenshots/`, named by the time (UTC).
#[cfg(not(target_arch = "wasm32"))]
fn save_screenshot() -> Result<PathBuf, String> {
    let dir = data_dir().ok_or("no data folder")?.join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let stamp = timestamp(secs);
    let path = (0..)
        .map(|n| match n {
            0 => dir.join(format!("reformers-{stamp}.png")),
            n => dir.join(format!("reformers-{stamp}-{n}.png")),
        })
        .find(|p| !p.exists())
        .unwrap();
    get_screen_data().export_png(&path.to_string_lossy());
    Ok(path)
}

/// Seconds since 1970 as `YYYY-MM-DD-hhmmss` (UTC; days to dates as in Hinnant's
/// `civil_from_days`).
fn timestamp(secs: u64) -> String {
    let (days, s) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02}-{:02}{:02}{:02}",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
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

/// Writes a replay into the data dir (`name` may include a subfolder); returns where.
fn save_replay(name: &str, replay: &Replay) -> Option<PathBuf> {
    let path = data_dir()?.join(name);
    let written = std::fs::create_dir_all(path.parent()?)
        .and_then(|_| std::fs::write(&path, replay.to_bytes()));
    match written {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!("can't save replay to {}: {e}", path.display());
            None
        }
    }
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
    /// Hash of MAP + COL (+ CHECK, OBJECTS): replays only replay on the files they were recorded on.
    hash: u64,
    /// (x, y, gate) for every painted gate cell.
    /// (x, y, zone) for every painted zone cell.
    zone_cells: Vec<(usize, usize, Zone)>,
    /// MAPTEX tile sheet for this map's palette: the map folder's own, or the shared one when
    /// the palettes match (TEREP1's BMP palette doesn't, so it draws flat colours).
    maptex: Option<Vec<u8>>,
    /// Trees and the like from the optional OBJECTS.PCX (#47), and the sprite sheet they're drawn
    /// from: the folder's own OBJTEX.PCX, else the shared one.
    objects: Vec<objects::Object>,
    objtex: Option<pcx::Image>,
    /// Each object kind's trunk (OBJTEX.TXT, else measured off the sprite).
    colliders: objects::Colliders,
    /// From the folder's optional INFO.TXT: the map's name (first line) and its credits (the rest).
    title: Option<String>,
    credits: String,
}

/// A map's files whose changes reload it while the game runs.
fn watched(map: &str) -> Vec<Option<std::time::SystemTime>> {
    [
        "MAP.PNG",
        "MAP.PGM",
        "MAP.PCX",
        "MAP.BMP",
        "COL.PCX",
        "COL.BMP",
        "CHECK.PCX",
        "MAPTEX.PCX",
        "OBJECTS.PCX",
        "OBJTEX.PCX",
        "OBJTEX.TXT",
    ]
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
            // colour 255 is the sky (each map's own), not a tile colour
            (pcx::decode(&shared).ok()?.palette[..255] == terrain.palette[..255]).then_some(shared)
        });
    let track = check.as_deref().map(Track::load).transpose()?;
    let objects_pcx = assets::read(&format!("maps/{name}/OBJECTS.PCX")).unwrap_or_default();
    let objects = if objects_pcx.is_empty() {
        vec![]
    } else {
        objects::load(&objects_pcx).map_err(|e| format!("maps/{name}/OBJECTS.PCX: {e}"))?
    };
    // the sheet and its colliders: the map's own, else the shared ones (a map may carry its
    // own OBJTEX.TXT for the shared sprites; its own sheet never takes the shared colliders)
    let own_sheet = assets::read(&format!("maps/{name}/OBJTEX.PCX")).ok();
    let txt = assets::read(&format!("maps/{name}/OBJTEX.TXT"))
        .ok()
        .or_else(|| {
            own_sheet
                .is_none()
                .then(|| assets::read("OBJTEX.TXT").ok())
                .flatten()
        });
    let objtex = own_sheet
        .or_else(|| assets::read("OBJTEX.PCX").ok())
        .and_then(|b| pcx::decode(&b).ok());
    let txt = txt.map(|b| String::from_utf8_lossy(&b).into_owned());
    let colliders = objects::colliders(txt.as_deref(), objtex.as_ref())
        .map_err(|e| format!("maps/{name}: {e}"))?;
    // what the trees are made of counts too, once there are trees
    let collider_bytes: Vec<u8> = if objects_pcx.is_empty() {
        vec![]
    } else {
        colliders
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    };
    let zone_cells = track.as_ref().map_or(vec![], |t| {
        (0..SIZE * SIZE)
            .filter_map(|i| {
                t.zone_at(i % SIZE, i / SIZE)
                    .map(|z| (i % SIZE, i / SIZE, z))
            })
            .collect()
    });
    let info = assets::read(&format!("maps/{name}/INFO.TXT")).unwrap_or_default();
    let info = String::from_utf8_lossy(&info);
    let mut lines = info.lines().map(str::trim);
    let level = Level {
        title: lines.next().filter(|l| !l.is_empty()).map(String::from),
        credits: lines.collect::<Vec<_>>().join(" ").trim().to_string(),
        track,
        // the trees are in both (they're hit now); without OBJECTS.PCX both stay what they were
        key: check
            .as_ref()
            .map(|c| track_hash(&[&map, &col, c, &objects_pcx, &collider_bytes])),
        hash: track_hash(&[
            &map,
            &col,
            check.as_deref().unwrap_or_default(),
            &objects_pcx,
            &collider_bytes,
        ]),
        zone_cells,
        maptex,
        objects,
        objtex,
        colliders,
    };
    Ok((terrain, level))
}

impl Level {
    /// The objects' colliders, for `Sim::set_trunks`.
    fn trunks(&self) -> Vec<Trunk> {
        self.objects
            .iter()
            .filter_map(|o| o.trunk(&self.colliders))
            .collect()
    }
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

/// World (x, y, z-up) → render space (y-up). The world is left-handed (PCX rows run down),
/// so y maps to +z; mapping it to −z would show the map mirrored.
/// `draw_text` on whole device pixels: placed between them (centred text lands on halves), its
/// thin strokes blur away on a 1× screen.
fn text_px(s: impl AsRef<str>, x: f32, y: f32, size: f32, c: Color) -> TextDimensions {
    let snap = |v: f32| (v * screen_dpi_scale()).round() / screen_dpi_scale();
    draw_text(s.as_ref(), snap(x), snap(y), size, c)
}

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

/// OBJTEX.PCX through its own palette, colour 0 clear; with each kind's sprite (see `objects`).
type Sprites = [Option<([usize; 4], [f32; 2])>; 16];
fn objtex_texture(level: &Level) -> (Texture2D, Sprites) {
    let Some(img) = &level.objtex else {
        return (Texture2D::from_rgba8(1, 1, &[0; 4]), [None; 16]);
    };
    let rgba: Vec<u8> = img
        .pixels
        .iter()
        .flat_map(|&i| {
            let [r, g, b] = img.palette[i as usize];
            [r, g, b, if i == 0 { 0 } else { 255 }]
        })
        .collect();
    let tex = Texture2D::from_rgba8(img.width as u16, img.height as u16, &rgba);
    tex.set_filter(FilterMode::Nearest);
    (tex, std::array::from_fn(|k| objects::sprite(img, k as u8)))
}

/// The objects within fog range of `eye` as upright sprites turned to face it (around the
/// vertical only, so trunks stay upright), standing on the ground.
fn object_meshes(
    level: &Level,
    terrain: &Terrain,
    sheet: &Texture2D,
    sprites: &Sprites,
    eye: Vec3,
) -> Vec<Mesh> {
    let (w, h) = (sheet.width(), sheet.height());
    let mut out: Vec<Mesh> = vec![];
    for o in &level.objects {
        let Some(([x, y, sw, sh], foot)) = sprites[o.kind as usize] else {
            continue;
        };
        let [x0, y0] = o.pos.map(f32::from);
        let base = v([x0, y0, terrain.height_at(x0, y0)]);
        let to_eye = vec3(eye.x - base.x, 0.0, eye.z - base.z);
        // ponytail: a fixed cull radius (the fog's far end); a grid lookup if maps get dense
        if to_eye.length() > FOG_RANGE[1] + 2.0 {
            continue;
        }
        let right = vec3(to_eye.z, 0.0, -to_eye.x).normalize_or(Vec3::X);
        let k = objects::PIXEL * o.scale() / S; // render units per sprite pixel
        // the foot sinks a pixel into the ground, so slopes don't show a gap under it
        let at = |px: f32, py: f32| {
            base + right * (px - foot[0]) * k + Vec3::Y * (foot[1] - py - 1.0) * k
        };
        let (u0, u1) = (x as f32 / w, (x + sw) as f32 / w);
        let (v0, v1) = (y as f32 / h, (y + sh) as f32 / h);
        let (sw, sh) = (sw as f32, sh as f32);
        let quad = [
            (at(0.0, 0.0), u0, v0),
            (at(sw, 0.0), u1, v0),
            (at(sw, sh), u1, v1),
            (at(0.0, sh), u0, v1),
        ];
        if out
            .last()
            .is_none_or(|m| m.vertices.len() + 4 > u16::MAX as usize)
        {
            out.push(Mesh {
                vertices: vec![],
                indices: vec![],
                texture: Some(sheet.clone()),
            });
        }
        let m = out.last_mut().unwrap();
        let i = m.vertices.len() as u16;
        m.indices.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
        m.vertices.extend(
            quad.iter()
                .map(|&(p, u, vv)| Vertex::new(p.x, p.y, p.z, u, vv, WHITE)),
        );
    }
    out
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
    // 16 texels of equal width per cell (as the original), never sampling past the tile
    vec2 st = (tile * 16.0 + clamp(fract(cell) * 16.0, 0.5, 15.5)) / 256.0;
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

/// Objects: the fog look, with colour 0 cut out (not blended, so they need no sorting).
const OBJECT_FRAGMENT: &str = "#version 100
precision mediump float;
varying lowp vec2 uv;
varying lowp vec4 shade;
varying mediump float fog;
uniform sampler2D Texture;
uniform vec3 FogColor;
void main() {
    vec4 c = texture2D(Texture, uv) * shade;
    if (c.a < 0.5) discard;
    gl_FragColor = vec4(mix(c.rgb, FogColor, fog), 1.0);
}
";

/// FOG.DAT: past ~10 cells the original remaps colours toward the palette's colour 255, fully
/// by ~34 (table index = average depth / 512 world units; levels 5–17 change colours, and 17 is
/// all colour 255). That colour is the sky too: sky blue in the stock palette, night, haze or
/// dusk in maps that set their own.
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
/// One band of terrain rows. Shading: the original's flat N·L, or `baked` lighting.
fn chunk_mesh(t: &Terrain, chunk: usize, baked: Option<&lighting::Baked>) -> Mesh {
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
            let colour = match baked {
                Some(b) => {
                    let [r, g, b] = b.light(x, y, lighting::sun_angle(t, x, y));
                    Color::new(r, g, b, 1.0)
                }
                None => Color::new(shade, shade, shade, 1.0),
            };
            // vertex (x, y) is the corner of cell (x, y): the quad to its right and below is that
            // cell, one COL colour / MAPTEX tile, as the original draws it (and as the physics
            // reads materials); texel centres here put the colour edges mid-quad
            let (u, w) = (x as f32 / SIZE as f32, y as f32 / SIZE as f32);
            vertices.push(Vertex::new(x as f32, h(x, y) / S, y as f32, u, w, colour));
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
/// Wheels are sprites facing `eye` (the camera, render space), as the original draws them, with
/// their full height along the wheel's widest extent on screen (`cam_up` breaks the tie face-on).
#[allow(clippy::too_many_arguments)]
fn car_meshes(
    model: &Car,
    atoms: &[[f32; 3]],
    steer: f32,
    palette: &[[u8; 3]; 256],
    ground: &dyn Fn(f32, f32) -> f32,
    skin: &Skin,
    eye: Vec3,
    cam_up: Vec3,
    use_skin: bool,
) -> [Mesh; 3] {
    let n = atoms.len();
    let w = |p: [f32; 3]| Vec3::from(p);
    let unit = |a: Vec3| a.normalize_or_zero();
    let side = unit(w(atoms[7]) - w(atoms[6]));
    let fwd = unit(w(atoms[4]) - w(atoms[6]) + w(atoms[5]) - w(atoms[7]));
    let steered = fwd * steer.sin() + side * steer.cos(); // like 0x11f0
    let body = v(atoms
        .iter()
        .fold(Vec3::ZERO, |m, &p| m + Vec3::from(p) / n as f32)
        .into());
    let entry = |e: u16| -> Vec3 {
        let e = e as usize;
        let world = if e < 2 * n {
            let [x, y, z] = atoms[e / 2];
            if e % 2 == 1 {
                vec3(x, y, ground(x, y) + 3.0)
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
    // where the eye is in the car's world (render space is y-up)
    let eye_world = [eye.x * S, eye.z * S, eye.y * S];
    for (i, prim) in model.prims.iter().enumerate() {
        // one side of a two-sided panel: only from the front, as the original's back-face tests
        // drew it (both under a depth buffer would flicker)
        if model.one_sided[i]
            && let Some(face) = car::face_atoms(prim, n)
        {
            let pts: Vec<[f32; 3]> = face.iter().map(|&a| atoms[a]).collect();
            let nrm = car::normal(&pts);
            let c = pts.iter().fold([0.0; 3], |s, p| {
                [0, 1, 2].map(|k| s[k] + p[k] / pts.len() as f32)
            });
            if (0..3).map(|k| nrm[k] * (eye_world[k] - c[k])).sum::<f32>() <= 0.0 {
                continue;
            }
        }
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
                let c = entry(*centre);
                // the axle out of the car (rear wheels' axle entries point at each other)
                let mut a = unit(entry(*axle) - c);
                if a.dot(c - body) < 0.0 {
                    a = -a;
                }
                let d = unit(c - eye);
                // 0 = looking down the axle at the hub … 8 = at the inner side, 22.5° apart
                let k = ((-d.dot(a)).clamp(-1.0, 1.0).acos() / (std::f32::consts::PI / 8.0)).round()
                    as usize;
                let f = frames[k.min(frames.len() - 1)];
                // the wheel's full diameter shows along axle × view: upright seen from the side
                // (like the original's never-rolling camera), along the car seen from above;
                // face-on the disc is round, and the camera's up settles which way is up
                let level = (cam_up - d * d.dot(cam_up)).normalize_or_zero();
                let tall = a.cross(d);
                let tall = if tall.dot(level) < 0.0 { -tall } else { tall };
                let up = (tall + level * 0.05).normalize_or(level);
                let right = d.cross(up).normalize_or(unit(v(side.into())));
                let (hw, hh) = (f.half[0] / S, f.half[1] / S);
                if use_skin {
                    // the sprites show the axle pointing left on screen: mirror when it points right
                    let order = if a.dot(right) > 0.0 {
                        [1, 0, 3, 2]
                    } else {
                        [0, 1, 2, 3]
                    };
                    let corners = [
                        -right * hw + up * hh,
                        right * hw + up * hh,
                        right * hw - up * hh,
                        -right * hw - up * hh,
                    ];
                    fan(
                        &mut wheels,
                        (0..4)
                            .map(|j| {
                                let p = c + corners[j];
                                let uv = f.uv[order[j]];
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
                    // an ellipse in the tyre's colour (sampled an eighth in from the face-on frame's edge, past the rim)
                    let [a0, _, b0, _] = frames[0].uv;
                    let tyre = mid(
                        &mut [[a0[0] * 0.875 + b0[0] * 0.125, (a0[1] + b0[1]) / 2.0]].into_iter(),
                    );
                    fan(
                        &mut wheels,
                        (0..12)
                            .map(|j| {
                                let (sa, ca) = (j as f32 * std::f32::consts::TAU / 12.0).sin_cos();
                                let p = c + right * (ca * hw) + up * (sa * hh);
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

/// The checkpoint guide (#34): a yellow arrow hovering over `car`, pointing at `target` (world
/// x, y), level, bobbing gently with `time`.
fn draw_guide(car: &SimCar, target: [f32; 2], time: f64) {
    let atoms = car.atom_positions();
    let [cx, cy, _] = car.centre();
    let roof = atoms.iter().map(|a| a[2]).fold(f32::MIN, f32::max);
    let bob = (time * 3.0).sin() as f32 * 0.05;
    let base = v([cx, cy, roof]) + vec3(0.0, 0.45 + bob, 0.0);
    let (dx, dz) = (target[0] - cx, target[1] - cy);
    let len = (dx * dx + dz * dz).sqrt();
    if len < 1.0 {
        return;
    }
    let (f, r) = (
        vec3(dx / len, 0.0, dz / len),
        vec3(-dz / len, 0.0, dx / len),
    );
    // in (right, forward) units of 0.55 render units: a head and a shaft, centred on the car
    let shape = [
        (0.0, 1.0),
        (0.75, 0.15),
        (0.28, 0.15),
        (0.28, -0.9),
        (-0.28, -0.9),
        (-0.28, 0.15),
        (-0.75, 0.15),
    ];
    let colour = Color::new(1.0, 0.85, 0.15, 0.9);
    let vertices = shape
        .iter()
        .map(|&(x, y)| {
            let p = base + (r * x + f * y) * 0.55;
            Vertex::new(p.x, p.y, p.z, 0.0, 0.0, colour)
        })
        .collect();
    // the head, then the shaft (both faces: macroquad doesn't cull)
    let indices = vec![0, 1, 6, 2, 3, 4, 2, 4, 5];
    draw_mesh(&Mesh {
        vertices,
        indices,
        texture: None,
    });
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

/// CAR1–CAR5.DAT.
const CARS: usize = 5;

const CAMERAS: [&str; 4] = ["chase", "bumper", "orbit (Q/E)", "top-down"];

/// The extra skins (file, name), read once.
fn skin_list() -> &'static [(String, String)] {
    static LIST: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    LIST.get_or_init(assets::skins)
}

/// A skin file's display name; "" is the car's own.
fn skin_name(file: &str) -> &str {
    skin_list()
        .iter()
        .find(|(f, _)| f == file)
        .map_or("own", |(_, n)| n.as_str())
}

/// The skin `step` along from `file` in the list, the car's own ("") first; Enter steps forward.
fn next_skin(file: &str, step: i32) -> String {
    let list = skin_list();
    let i = list
        .iter()
        .position(|(f, _)| f == file)
        .map_or(0, |i| i + 1) as i32;
    let n = list.len() as i32 + 1;
    match (i + if step == 0 { 1 } else { step }).rem_euclid(n) {
        0 => String::new(),
        k => list[k as usize - 1].0.clone(),
    }
}

/// Loads skin `file` into `cache` (once; None if it won't load).
fn cache_skin(
    cache: &mut std::collections::HashMap<String, Option<Skin>>,
    file: &str,
    palette: &[[u8; 3]; 256],
) {
    if !file.is_empty() && !cache.contains_key(file) {
        let skin = assets::read(&format!("skins/{file}"))
            .ok()
            .filter(|b| pcx::decode(b).is_ok())
            .map(|b| skin_texture(&b, palette));
        cache.insert(file.to_string(), skin);
    }
}

/// Car `n` in skin `file` (from `cache`), or in its own skin.
fn skin_of<'a>(
    cache: &'a std::collections::HashMap<String, Option<Skin>>,
    cars: &'a [(Car, Skin)],
    n: usize,
    file: &str,
) -> &'a Skin {
    cache
        .get(file)
        .and_then(Option::as_ref)
        .unwrap_or(&cars[n].1)
}

/// Ground that squeals rather than crunches when a tyre slides on it: grey (asphalt, concrete,
/// rock), judged by its colour, as which material code means what isn't known (re-notes).
fn hard_ground(rgb: [u8; 3]) -> bool {
    let (hi, lo) = (*rgb.iter().max().unwrap(), *rgb.iter().min().unwrap());
    hi > 40 && (hi - lo) as f32 <= hi as f32 * 0.2
}

/// Getting an online race together (#33): listening for a player, or asking a host.
enum Lobby {
    Off,
    Hosting(net::Host),
    Joining(net::Joiner),
}

/// Graphics options: one field + one menu row each; saved to settings.txt.
struct Graphics {
    /// Map folder name (switching it in the menu loads that map).
    map: String,
    /// 1, or 2 for split-screen (not saved: a session choice).
    players: usize,
    /// Player 1's and player 2's car, 0-based (CAR1–CAR5).
    car: usize,
    car2: usize,
    /// Player 1's and player 2's skin file in `skins/`; empty = the car's own.
    skin: String,
    skin2: String,
    /// The arrow over the car towards the next checkpoint (#34).
    guide: bool,
    /// Repair mends while held, from a recharging budget (#58), instead of a fresh car at once.
    gradual: bool,
    /// Player 1 steers with WASD instead of the arrows (one player only: two share the keyboard).
    wasd: bool,
    /// The last address typed on the Join row (saved), and the online rows' status (not saved).
    join: String,
    host_label: String,
    join_label: String,
    camera: usize, // index into CAMERAS
    nearest: bool,
    textured: bool,
    /// Baked terrain lighting (`lighting.rs`) instead of the original's flat shading.
    lighting: bool,
    crt: bool,
    minimap: bool,
    ghost: bool,
    fog: bool,
    volume: u8,    // 0 (mute) – 10
    engine: usize, // index into ENGINE_SOUNDS
    fullscreen: bool,
    resolution: usize, // index into RESOLUTIONS, windowed mode only
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Resume,
    Map,
    Players,
    Car,
    Skin,
    Car2,
    Skin2,
    Sound,
    EngineSound,
    Camera,
    Filter,
    Textures,
    Lighting,
    Crt,
    Fog,
    Minimap,
    Ghost,
    Guide,
    Repair,
    Keys,
    Host,
    Join,
    Fullscreen,
    Resolution,
    Quit,
}
/// Menu pages (Esc menu): the top level and its submenus.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    Top,
    Race,
    Graphics,
    Sound,
    Controls,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Item {
    Row(Row),
    Open(Page),
    Text(&'static str),
}

const CONTROLS: [&str; 13] = [
    "Arrows (or WASD, above) / left stick: drive",
    "Space / B: brake",
    "R / Y: back on the wheels (last checkpoint)",
    "F / Back: repair (gradual: hold; double-tap for all)",
    "T / left shoulder: turn over where it stands",
    "C / right shoulder: camera (Q / E orbit)",
    "1-5: switch car",
    "W: spring wireframe (with the arrows)",
    "P: save replay",
    "F12: screenshot",
    "Esc / Start: this menu",
    "Player 2: WASD, Shift brake, Tab R, G repair, H turn over",
    "Online: Race > Host / Join (--host, --join ADDRESS)",
];

/// The page's items; `players` adds player 2's car row.
fn items(page: Page, players: usize) -> Vec<Item> {
    use Item::{Open, Row as R};
    match page {
        Page::Top => vec![
            R(Row::Resume),
            Open(Page::Race),
            Open(Page::Graphics),
            Open(Page::Sound),
            Open(Page::Controls),
            R(Row::Quit),
        ],
        Page::Race => [
            Row::Map,
            Row::Players,
            Row::Car,
            Row::Skin,
            Row::Car2,
            Row::Skin2,
        ]
        .into_iter()
        .filter(|&r| !matches!(r, Row::Car2 | Row::Skin2) || players == 2)
        .chain([
            Row::Camera,
            Row::Ghost,
            Row::Minimap,
            Row::Guide,
            Row::Repair,
            Row::Host,
            Row::Join,
        ])
        .map(R)
        .collect(),
        Page::Graphics => [
            Row::Textures,
            Row::Lighting,
            Row::Filter,
            Row::Crt,
            Row::Fog,
            Row::Fullscreen,
            Row::Resolution,
        ]
        .map(R)
        .to_vec(),
        Page::Sound => vec![R(Row::Sound), R(Row::EngineSound)],
        Page::Controls => std::iter::once(R(Row::Keys))
            .chain(CONTROLS.map(Item::Text))
            .collect(),
    }
}

#[derive(Debug, PartialEq)]
enum MenuAction {
    None,
    Close,
    Quit,
    /// Change a setting: −1 / +1 for left / right, 0 for Enter.
    Change(Row, i32),
}

/// Where the menu is: page, selected row, and the drum's eased scroll position.
struct Menu {
    page: Page,
    /// 1 or 2, as `Graphics::players`: whether player 2's car row is listed.
    players: usize,
    selected: usize,
    scroll: f32,
}

impl Menu {
    fn new() -> Menu {
        Menu {
            page: Page::Top,
            players: 1,
            selected: 0,
            scroll: 0.0,
        }
    }

    /// One frame of menu keys. `back` is Esc / Backspace.
    fn input(
        &mut self,
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        enter: bool,
        back: bool,
    ) -> MenuAction {
        let list = items(self.page, self.players);
        if up && self.selected > 0 {
            self.selected -= 1;
        }
        if down && self.selected + 1 < list.len() {
            self.selected += 1;
        }
        let go_back = |m: &mut Menu| {
            let from = m.page;
            m.page = Page::Top;
            m.selected = items(Page::Top, 1)
                .iter()
                .position(|&i| i == Item::Open(from))
                .unwrap_or(0);
            m.scroll = m.selected as f32;
        };
        match list[self.selected] {
            Item::Row(Row::Resume) if enter => return MenuAction::Close,
            Item::Row(Row::Quit) if enter => return MenuAction::Quit,
            Item::Row(Row::Resume | Row::Quit) => {}
            Item::Row(row) if left || right || enter => {
                return MenuAction::Change(row, right as i32 - left as i32);
            }
            Item::Open(p) if enter || right => {
                self.page = p;
                self.selected = 0;
                self.scroll = 0.0;
                return MenuAction::None;
            }
            _ if left && self.page != Page::Top => {
                go_back(self);
                return MenuAction::None;
            }
            _ => {}
        }
        if back {
            if self.page == Page::Top {
                return MenuAction::Close;
            }
            go_back(self);
        }
        MenuAction::None
    }
}

impl Graphics {
    fn to_text(&self) -> String {
        format!(
            "map={}\ncar={}\ncar2={}\nskin={}\nskin2={}\njoin={}\nguide={}\ngradual={}\nwasd={}\ncamera={}\nnearest={}\ntextured={}\nlighting={}\ncrt={}\nminimap={}\nghost={}\nfog={}\nvolume={}\nengine={}\nfullscreen={}\nresolution={}\n",
            self.map,
            self.car + 1,
            self.car2 + 1,
            self.skin,
            self.skin2,
            self.join,
            self.guide,
            self.gradual,
            self.wasd,
            self.camera,
            self.nearest,
            self.textured,
            self.lighting,
            self.crt,
            self.minimap,
            self.ghost,
            self.fog,
            self.volume,
            self.engine,
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
                "skin" => self.skin = v.trim().to_string(),
                "skin2" => self.skin2 = v.trim().to_string(),
                "join" => self.join = v.trim().to_string(),
                "guide" => self.guide = b,
                "gradual" => self.gradual = b,
                "wasd" => self.wasd = b,
                "car" => {
                    self.car = n
                        .filter(|n| (1..=CARS).contains(n))
                        .map_or(self.car, |n| n - 1)
                }
                "car2" => {
                    self.car2 = n
                        .filter(|n| (1..=CARS).contains(n))
                        .map_or(self.car2, |n| n - 1)
                }
                "camera" => self.camera = n.filter(|&n| n < CAMERAS.len()).unwrap_or(self.camera),
                "nearest" => self.nearest = b,
                "textured" => self.textured = b,
                "lighting" => self.lighting = b,
                "crt" => self.crt = b,
                "minimap" => self.minimap = b,
                "ghost" => self.ghost = b,
                "fog" => self.fog = b,
                "volume" => self.volume = n.filter(|&n| n <= 10).map_or(self.volume, |n| n as u8),
                "engine" => {
                    self.engine = n
                        .filter(|&n| n < ENGINE_SOUNDS.len())
                        .unwrap_or(self.engine)
                }
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
            Row::Car if self.players == 2 => format!("P1 car: {}", self.car + 1),
            Row::Car => format!("Car: {}", self.car + 1),
            Row::Car2 => format!("P2 car: {}", self.car2 + 1),
            Row::Skin if self.players == 2 => format!("P1 skin: {}", skin_name(&self.skin)),
            Row::Skin => format!("Skin: {}", skin_name(&self.skin)),
            Row::Skin2 => format!("P2 skin: {}", skin_name(&self.skin2)),
            Row::Host => self.host_label.clone(),
            Row::Join => self.join_label.clone(),
            Row::Players => match self.players {
                1 => "Players: 1".into(),
                _ => "Players: 2  (P2: WASD, Shift brake, Tab R, G repair, H turn over)".into(),
            },
            Row::EngineSound => format!("Engine sound: {}", ENGINE_SOUNDS[self.engine]),
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
            Row::Lighting => format!(
                "Lighting: {}",
                if self.lighting {
                    "baked (shadows, sky, bounce)"
                } else {
                    "original"
                }
            ),
            Row::Crt => format!("CRT filter: {}", on(self.crt)),
            Row::Minimap => format!("Minimap: {}", on(self.minimap)),
            Row::Guide => format!("Checkpoint arrow: {}", on(self.guide)),
            Row::Repair => format!(
                "Repair: {}",
                if self.gradual {
                    "gradual, from a budget"
                } else {
                    "instant"
                }
            ),
            Row::Keys => format!(
                "Steering keys: {}",
                if self.wasd {
                    "WASD  (one player)"
                } else {
                    "arrows"
                }
            ),
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
            Row::Lighting => self.lighting = !self.lighting,
            Row::Crt => self.crt = !self.crt,
            Row::Minimap => self.minimap = !self.minimap,
            Row::Guide => self.guide = !self.guide,
            Row::Repair => self.gradual = !self.gradual,
            Row::Keys => self.wasd = !self.wasd,
            Row::Fog => self.fog = !self.fog,
            Row::Players => self.players = 3 - self.players,
            Row::Car => {
                self.car = (self.car as i32 + if step == 0 { 1 } else { step })
                    .rem_euclid(CARS as i32) as usize
            }
            Row::Skin => self.skin = next_skin(&self.skin, step),
            Row::Skin2 => self.skin2 = next_skin(&self.skin2, step),
            Row::Car2 => {
                self.car2 = (self.car2 as i32 + if step == 0 { 1 } else { step })
                    .rem_euclid(CARS as i32) as usize
            }
            Row::EngineSound => self.engine = (self.engine + 1) % ENGINE_SOUNDS.len(),
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
            Row::Resume | Row::Quit | Row::Host | Row::Join => return false,
        }
        true
    }
}

/// COL.PCX at (x0, y0), `size` square, with the zones and gates on it (`next` gate yellow, start white).
fn draw_map_view(
    colour: &Texture2D,
    zones: &[(usize, usize, Zone)],
    track: Option<&Track>,
    next: Option<usize>,
    x0: f32,
    y0: f32,
    size: f32,
) {
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
        for (g, piece) in t
            .gates
            .iter()
            .enumerate()
            .flat_map(|(g, ps)| ps.iter().map(move |p| (g, p)))
        {
            let (a, b) = (to_map(piece.ends[0]), to_map(piece.ends[1]));
            let (w, c) = match g {
                _ if Some(g) == next => (4.0, YELLOW),
                0 => (2.5, WHITE),
                _ => (2.0, Color::new(1.0, 1.0, 1.0, 0.5)),
            };
            draw_line(a.x, a.y, b.x, b.y, w, c);
        }
    }
}

/// COL.PCX in the top-right corner with the gates (next one yellow, start white), the ghost and the car.
#[allow(clippy::too_many_arguments)]
fn draw_minimap(
    colour: &Texture2D,
    zones: &[(usize, usize, Zone)],
    track: Option<&Track>,
    next: usize,
    car: &SimCar,
    others: &[&SimCar],
    ghost: Option<[f32; 2]>,
    top: f32,
) {
    let size = (screen_width().min(screen_height()) * 0.28).round();
    let (x0, y0) = (screen_width() - size - 12.0, top + 12.0);
    let to_map = |[x, y]: [f32; 2]| vec2(x0 + x / 65536.0 * size, y0 + y / 65536.0 * size);
    draw_map_view(colour, zones, track, Some(next), x0, y0, size);
    for o in others {
        let [x, y, _] = o.centre();
        let p = to_map([x, y]);
        draw_circle(p.x, p.y, 4.0, SKYBLUE);
    }
    if let Some(g) = ghost {
        let g = to_map(g);
        draw_circle(g.x, g.y, 4.0, Color::new(1.0, 1.0, 1.0, 0.6));
    }
    let [x, y, _] = car.centre();
    let p = to_map([x, y]);
    let (sin, cos) = (car.heading() as f32 / 65536.0 * std::f32::consts::TAU).sin_cos();
    let nose = p + vec2(sin, -cos) * 9.0;
    draw_line(p.x, p.y, nose.x, nose.y, 2.0, RED);
    draw_circle(p.x, p.y, 4.0, RED);
}

/// Where the menu's preview goes: a square right of the (shifted) drum.
fn preview_rect() -> Rect {
    let (w, h) = (screen_width(), screen_height());
    let s = (w * 0.38).min(h * 0.6).round();
    Rect::new(
        (w * 0.75 - s / 2.0).round(),
        (h * 0.5 - s / 2.0).round(),
        s,
        s,
    )
}

/// The map's minimap (no cars) with its name under it.
fn draw_map_preview(name: &str, colour: &Texture2D, level: &Level) {
    let r = preview_rect();
    draw_map_view(
        colour,
        &level.zone_cells,
        level.track.as_ref(),
        None,
        r.x,
        r.y,
        r.w,
    );
    let title = level.title.as_deref().unwrap_or(name);
    let caption = if level.track.is_some() {
        title.to_string()
    } else {
        format!("{title}  (free drive)")
    };
    let centred = |text: &str, y: f32, size: f32, colour: Color| {
        let dims = measure_text(text, None, size as u16, 1.0);
        text_px(text, r.x + (r.w - dims.width) / 2.0, y, size, colour);
    };
    centred(&caption, r.y + r.w + 32.0, 24.0, WHITE);
    // the credits, cut to the preview's width
    let mut credits: Vec<char> = level.credits.chars().collect();
    let fits = |c: &[char]| {
        measure_text(c.iter().collect::<String>().as_str(), None, 18, 1.0).width <= r.w
    };
    if !fits(&credits) {
        while !credits.is_empty() && !fits(&[&credits[..], &['…']].concat()) {
            credits.pop();
        }
        credits.push('…');
    }
    centred(
        &credits.iter().collect::<String>(),
        r.y + r.w + 58.0,
        18.0,
        GRAY,
    );
}

/// The car at rest on a turntable, rendered through `target` and drawn into `preview_rect`.
#[allow(clippy::too_many_arguments)]
fn draw_car_preview(
    target: &RenderTarget,
    caption: &str,
    model: &Car,
    skin: &Skin,
    palette: &[[u8; 3]; 256],
    textured: bool,
    angle: f32,
) {
    // car-local atoms (front is −y), turned about their middle; the ground under the wheels
    let n = model.atoms.len() as f32;
    let (mx, my) = model
        .atoms
        .iter()
        .fold((0.0, 0.0), |(x, y), a| (x + a.pos[0] / n, y + a.pos[1] / n));
    let floor = model
        .atoms
        .iter()
        .map(|a| a.pos[2] - a.radius)
        .fold(f32::MAX, f32::min);
    let (sa, ca) = angle.sin_cos();
    let atoms: Vec<[f32; 3]> = model
        .atoms
        .iter()
        .map(|a| {
            let (x, y) = (a.pos[0] - mx, a.pos[1] - my);
            [x * ca - y * sa, x * sa + y * ca, a.pos[2] - floor]
        })
        .collect();
    let reach = atoms
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt())
        .fold(0.0, f32::max)
        / S;
    let look = vec3(0.0, reach * 0.3, 0.0);
    let eye = look + vec3(0.0, reach * 1.1, reach * 3.2);
    set_camera(&Camera3D {
        position: eye,
        target: look,
        up: Vec3::Y,
        aspect: Some(1.0),
        render_target: Some(target.clone()),
        ..Default::default()
    });
    clear_background(Color::new(0.0, 0.0, 0.0, 0.0));
    gl_use_default_material();
    for mesh in car_meshes(
        model,
        &atoms,
        0.0,
        palette,
        &|_, _| -3.0, // shadow right on the turntable
        skin,
        eye,
        Vec3::Y,
        textured,
    ) {
        draw_mesh(&mesh);
    }
    set_default_camera();
    let r = preview_rect();
    draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.0, 0.0, 0.0, 0.35));
    draw_texture_ex(
        &target.texture,
        r.x,
        r.y,
        WHITE,
        DrawTextureParams {
            dest_size: Some(vec2(r.w, r.h)),
            flip_y: true,
            ..Default::default()
        },
    );
    let dims = measure_text(caption, None, 24, 1.0);
    text_px(
        caption,
        r.x + (r.w - dims.width) / 2.0,
        r.y + r.w + 32.0,
        24.0,
        WHITE,
    );
}

/// The menu as a drum seen side-on: rows on a cylinder, the selected one centred and full size,
/// the others shrinking and fading out with their angle; scrolling eases.
fn draw_menu(gfx: &Graphics, menu: &mut Menu, dt: f32) {
    menu.scroll += (menu.selected as f32 - menu.scroll) * (dt * 12.0).min(1.0);
    let (w, h) = (screen_width(), screen_height());
    draw_rectangle(0.0, 0.0, w, h, Color::new(0.0, 0.0, 0.0, 0.6));
    let title = match menu.page {
        Page::Top => "PAUSED".to_string(),
        p => format!("PAUSED  >  {p:?}"),
    };
    // the Race page sits left of centre, leaving the right for its map / car previews
    let cx = if menu.page == Page::Race {
        w * 0.32
    } else {
        w / 2.0
    };
    let size = measure_text(&title, None, 40, 1.0);
    text_px(&title, cx - size.width / 2.0, h * 0.14, 40.0, WHITE);
    let (cy, radius, step) = (h * 0.52, h * 0.3, 0.18f32);
    let list = items(menu.page, gfx.players);
    for (i, item) in list.iter().enumerate() {
        let angle = (i as f32 - menu.scroll) * step;
        if angle.abs() > 1.4 {
            continue;
        }
        let near = angle.cos();
        let chosen = i == menu.selected;
        let (text, base) = match item {
            Item::Row(r) => (gfx.label(*r), 34.0),
            Item::Open(p) => (format!("{p:?}  >"), 34.0),
            Item::Text(t) => (t.to_string(), 26.0),
        };
        let font = (base * near).max(10.0);
        let dims = measure_text(&text, None, font as u16, 1.0);
        let y = cy + radius * angle.sin() + dims.height / 2.0;
        let fade = near.powf(6.0); // rows melt away towards the drum's top and bottom
        let colour = if chosen {
            Color::new(1.0, 0.9, 0.2, fade)
        } else if matches!(item, Item::Text(_)) {
            Color::new(0.8, 0.8, 0.8, fade)
        } else {
            Color::new(1.0, 1.0, 1.0, fade * 0.85)
        };
        text_px(&text, cx - dims.width / 2.0, y, font, colour);
        if chosen {
            let bar = dims.width + 40.0;
            draw_rectangle_lines(
                cx - bar / 2.0,
                y - dims.height - 8.0,
                bar,
                dims.height + 16.0,
                2.0,
                colour,
            );
        }
    }
    let hint = match menu.page {
        Page::Top => "up/down select   enter open   esc resume",
        _ => "up/down select   left/right/enter change   esc back",
    };
    let size = measure_text(hint, None, 18, 1.0);
    text_px(hint, cx - size.width / 2.0, h * 0.93, 18.0, GRAY);
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
            icon: Some(reformers::icon::icon()),
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
    assets::preload().await;
    let args: Vec<String> = std::env::args().collect();
    let car_flag = args
        .iter()
        .position(|a| a == "--car")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let read = |f: &str| assets::read(f).unwrap_or_else(|e| panic!("{e}"));
    let saved = settings_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    // --replay FILE: re-drive a recorded run (its map, car and start), then hand over the keys
    let replay = args
        .iter()
        .position(|a| a == "--replay")
        .and_then(|i| args.get(i + 1))
        .map(|f| {
            let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("{f}: {e}"));
            Replay::from_bytes(&bytes).unwrap_or_else(|e| panic!("{f}: {e}"))
        });
    let mut map = match &replay {
        Some(r) if assets::maps().contains(&r.map) => r.map.clone(),
        Some(r) => panic!("the replay is on map {:?}, which isn't installed", r.map),
        None => pick_map(&args, &saved),
    };
    let (terrain, mut level) = load_level(&map).unwrap_or_else(|e| panic!("{e}"));
    let mut sim = Sim::new(terrain);
    if let Some(t) = &level.track {
        sim.set_soft_dirt(t.soft_dirt.clone());
    }
    sim.set_trunks(level.trunks());
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
    // --car N, else the car saved in settings.txt, else car 1
    let saved_car = saved.lines().find_map(|l| l.strip_prefix("car="));
    let mut current = car_flag
        .as_deref()
        .or(saved_car)
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, CARS)
        - 1;
    let mut wireframe = false;
    let mut speed = 0.0f32;
    sim.dig_everywhere = args.iter().any(|a| a == "--soft");
    let (spawn, spawn_heading) = level
        .track
        .as_ref()
        .and_then(|t| t.spawn.as_ref())
        .map_or((SPAWN, 0), |s| ((s.x, s.y), s.heading));
    let (spawn, spawn_heading) = match &replay {
        Some(r) => {
            if r.key != level.hash {
                panic!(
                    "the replay was recorded on a different version of map {:?}",
                    r.map
                );
            }
            current = (r.car.clamp(1, 5) - 1) as usize;
            sim.dig_everywhere = r.soft;
            sim.gradual_repair = r.gradual;
            ((r.start[0], r.start[1]), r.start[2])
        }
        None => (spawn, spawn_heading),
    };
    sim.cars.push(SimCar::spawn_facing(
        &cars[current].0,
        &sim.terrain,
        spawn.0,
        spawn.1,
        spawn_heading,
    ));
    let mut players = vec![Player::new(
        current,
        Race::new(best_key(&level, current).and_then(|k| load_best(&k))),
        v(sim.cars[0].centre()) + vec3(0.0, 2.0, 4.0),
    )];
    let mut ghost = load_ghost(&level, players[0].car);
    let mut recording = Ghost::default(); // this lap so far
    let mut ticks = 0u64;
    // this run's inputs since the car was placed (a replay), and the replay being played back
    let new_run = |map: &str, level: &Level, car: usize, sim: &Sim| {
        let c = &sim.cars[0];
        let [x, y, _] = c.centre();
        Replay {
            map: map.to_string(),
            key: level.hash,
            car: car as u8 + 1,
            soft: sim.dig_everywhere,
            gradual: sim.gradual_repair,
            start: [x as u16, y as u16, c.heading()],
            inputs: vec![],
        }
    };
    let mut recording_run = new_run(&map, &level, players[0].car, &sim);
    recording_run.start = [spawn.0, spawn.1, spawn_heading]; // exactly where it was placed
    let mut playing = replay.map(|r| (r, 0usize));
    let mut pristine = sim.terrain.clone(); // the map as loaded, for starting new runs on it
    let mut banner: Option<(String, f64)> = None; // message, shown until
    let mut last_centre = sim.cars[0].centre();

    let chunks = SIZE.div_ceil(CHUNK);
    let mut meshes: Vec<Mesh> = (0..chunks)
        .map(|c| chunk_mesh(&sim.terrain, c, None))
        .collect();
    // baked lighting for the loaded map, while the option is on (made on the first frame)
    let mut baked: Option<lighting::Baked> = None;
    let colour_tex = Texture2D::from_image(&colour_image(&sim.terrain));
    let mut maptex = tiles(&level, &sim.terrain.palette);
    let terrain_material = fog_material(
        TERRAIN_VERTEX,
        TERRAIN_FRAGMENT,
        vec!["ColIdx".into(), "MapTex".into()],
    );
    let fog = fog_material(TERRAIN_VERTEX, FOG_FRAGMENT, vec![]);
    let object_material = fog_material(TERRAIN_VERTEX, OBJECT_FRAGMENT, vec![]);
    let (mut objtex, mut sprites) = objtex_texture(&level);
    terrain_material.set_texture("MapTex", maptex.clone());
    let index_tex = Texture2D::from_image(&index_image(&sim.terrain));
    index_tex.set_filter(FilterMode::Nearest);
    terrain_material.set_texture("ColIdx", index_tex.clone());
    let mut shown_colour = sim.terrain.colour.clone();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let mut gfx = Graphics {
        map: map.clone(),
        car: 0,
        car2: 1,
        skin: String::new(),
        skin2: String::new(),
        join: String::new(),
        guide: true,
        gradual: true,
        wasd: false,
        host_label: String::new(),
        join_label: String::new(),
        players: if args.windows(2).any(|w| w[0] == "--players" && w[1] == "2") {
            2
        } else {
            1
        },
        camera: 0,
        nearest: true,
        textured: true,
        lighting: false,
        crt: false,
        minimap: true,
        ghost: false,
        fog: true,
        volume: 7,
        engine: 0,
        fullscreen: false,
        resolution: 1,
    };
    gfx.apply_text(&saved);
    gfx.map = map.clone(); // pick_map already weighed --map and the saved one
    gfx.car = players[0].car; // so did --car (and a replay picks its own)
    // launch flags win over the saved settings
    gfx.nearest &= !flag("--smooth");
    gfx.textured &= !flag("--untextured");
    gfx.lighting |= flag("--baked-light");
    gfx.crt |= flag("--crt");
    gfx.minimap &= !flag("--no-minimap");
    gfx.ghost |= flag("--ghost");
    gfx.fog &= !flag("--no-fog");
    if flag("--mute") {
        gfx.volume = 0;
    }
    let mut audio = Audio::load().await;
    #[cfg(not(target_arch = "wasm32"))]
    let mut gilrs = Gilrs::new().ok();
    #[cfg(not(target_arch = "wasm32"))]
    let mut pad_id: Option<(GamepadId, Rested)> = None;
    #[cfg(target_arch = "wasm32")]
    let mut pad_id: Option<()> = None;
    #[cfg(target_arch = "wasm32")]
    let mut gilrs: Option<()> = None;
    let (mut impacts, mut last_thud, mut skid) = (0u32, 0.0f64, [0.0f32; 2]);
    let mut revs_smooth = [48.0f32; 2]; // per player, in F-number units
    let mut audio_rec = args
        .iter()
        .position(|a| a == "--record-audio")
        .and_then(|i| args.get(i + 1))
        .map(|p| AudioRecording {
            path: p.clone(),
            out: vec![],
            thuds: vec![],
            last_write: 0.0,
        });
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
    let mut menu = Menu::new();
    let mut skins: std::collections::HashMap<String, Option<Skin>> = Default::default();
    // online race (#33): getting together, the running link with what it started from, a race
    // to set up once its map is loaded, hosts found on the LAN, and why we're leaving one
    let mut lobby = Lobby::Off;
    let mut go = false; // the host pressed start
    let mut gone_seen: Vec<bool> = vec![]; // online: who we've said left
    let mut online: Option<(net::Link, net::Start)> = None;
    let mut starting: Option<net::Start> = None;
    let mut force_load = false;
    let mut discovery: Option<net::Discovery> = None;
    let mut leave: Option<String> = None;
    // --host [N] / --join ADDRESS: straight into the lobby; with N, the race starts by itself
    // once N cars are in
    let auto_start = args
        .iter()
        .position(|a| a == "--host")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<usize>().ok());
    if args.iter().any(|a| a == "--host") {
        match net::Host::bind(net::PORT) {
            Ok(h) => lobby = Lobby::Hosting(h),
            Err(e) => banner = Some((format!("can't host: {e}"), get_time() + 5.0)),
        }
    }
    if let Some(addr) = args
        .iter()
        .position(|a| a == "--join")
        .and_then(|i| args.get(i + 1))
    {
        use std::net::ToSocketAddrs;
        let addr = if addr.contains(':') {
            addr.clone()
        } else {
            format!("{addr}:{}", net::PORT)
        };
        match addr.to_socket_addrs().ok().and_then(|mut a| a.next()) {
            Some(a) => match net::Joiner::new(a, players[0].car as u8, &gfx.skin) {
                Ok(j) => lobby = Lobby::Joining(j),
                Err(e) => banner = Some((format!("can't join: {e}"), get_time() + 5.0)),
            },
            None => banner = Some((format!("no such address: {addr}"), get_time() + 5.0)),
        }
    }
    // menu previews: each map's minimap (loaded on first look), and the car turntable's target
    let mut map_previews: std::collections::HashMap<String, Option<(Texture2D, Level)>> =
        Default::default();
    let car_target = render_target_ex(
        512,
        512,
        RenderTargetParams {
            sample_count: 1,
            depth: true,
        },
    );
    let mut shown = sim.terrain.height.clone();
    let mut original = shown.clone();
    let mut acc = 0.0;
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let started = get_time();

    loop {
        let (pad_held, pad_hit) = poll_pad(&mut gilrs, &mut pad_id);
        let pressed = |k: KeyCode| is_key_pressed(k) || pad_hit.contains(&k);
        let down = |k: KeyCode| is_key_down(k) || pad_held.contains(&k);
        // online: players gather in the host's lobby; the host starts (Enter on Host), the
        // joiners get the start
        let mut begin = None;
        match &mut lobby {
            Lobby::Hosting(h) => {
                h.poll(&map);
                go |= auto_start.is_some_and(|n| h.guests.len() + 1 >= n.min(net::MAX_PLAYERS));
            }
            Lobby::Joining(j) => match j.poll() {
                net::JoinState::Waiting => {}
                net::JoinState::Started(s) => begin = Some(s),
                net::JoinState::Failed(why) => {
                    banner = Some((format!("can't join: {why}"), get_time() + 5.0));
                    lobby = Lobby::Off;
                }
            },
            Lobby::Off => {}
        }
        if std::mem::take(&mut go)
            && let Lobby::Hosting(h) = std::mem::replace(&mut lobby, Lobby::Off)
        {
            // the host decides everything, the grid included (worked out here, sent as numbers)
            let ((sx, sy), sh) = level
                .track
                .as_ref()
                .and_then(|t| t.spawn.as_ref())
                .map_or((SPAWN, 0), |s| ((s.x, s.y), s.heading));
            let cars: Vec<u8> = std::iter::once(players[0].car as u8)
                .chain(h.guests.iter().map(|g| g.car.min(CARS as u8 - 1)))
                .collect();
            let start = net::Start {
                map: map.clone(),
                map_hash: level.hash,
                soft: sim.dig_everywhere,
                gradual: sim.gradual_repair,
                skins: std::iter::once(gfx.skin.clone())
                    .chain(h.guests.iter().map(|g| g.skin.clone()))
                    .collect(),
                spawns: (0..cars.len())
                    .map(|i| {
                        let (x, y) = grid((sx, sy), sh, i);
                        [x, y, sh]
                    })
                    .collect(),
                cars,
                you: 0,
            };
            online = Some((h.start(start.clone()), start.clone()));
            begin = Some(start);
        } else if let Some(s) = &begin
            && let Lobby::Joining(j) = std::mem::replace(&mut lobby, Lobby::Off)
        {
            online = Some((j.start(s), s.clone()));
        }
        if let Some(s) = begin {
            // both load the map fresh (undoing any digging) before the cars go down
            gfx.map = s.map.clone();
            force_load = true;
            starting = Some(s);
        }
        // hot reload: edit a map image, save, and it's here within a second
        // a map picked in the menu loads when the menu closes, not on every step through the list;
        // online, the map only changes when a race starts
        let want = if (menu_open || online.is_some()) && !force_load {
            &map
        } else {
            &gfx.map
        }
        .clone();
        if get_time() >= next_poll || want != map || force_load {
            next_poll = get_time() + 1.0;
            let now = watched(&want);
            let switched = want != map || force_load;
            force_load = false;
            if (now != stamps && online.is_none()) || switched {
                stamps = now;
                match load_level(&want) {
                    Ok((terrain, new)) => {
                        sim.set_terrain(terrain);
                        level = new;
                        if let Some(t) = &level.track {
                            sim.set_soft_dirt(t.soft_dirt.clone());
                        }
                        sim.set_trunks(level.trunks());
                        map = want;
                        baked = gfx.lighting.then(|| lighting::bake(&sim.terrain));
                        meshes = (0..chunks)
                            .map(|c| chunk_mesh(&sim.terrain, c, baked.as_ref()))
                            .collect();
                        colour_tex.update(&colour_image(&sim.terrain));
                        index_tex.update(&index_image(&sim.terrain));
                        maptex = tiles(&level, &sim.terrain.palette);
                        terrain_material.set_texture("MapTex", maptex.clone());
                        (objtex, sprites) = objtex_texture(&level);
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
                                .map_or((SPAWN, 0), |s| ((s.x, s.y), s.heading));
                            sim.cars[0] = SimCar::spawn_facing(
                                &cars[players[0].car].0,
                                &sim.terrain,
                                sx,
                                sy,
                                h,
                            );
                            players[0].roll = [0.0; 4];
                            if sim.cars.len() > 1 {
                                let (bx, by) = grid((sx, sy), h, 1);
                                sim.cars[1] = SimCar::spawn_facing(
                                    &cars[players[1].car].0,
                                    &sim.terrain,
                                    bx,
                                    by,
                                    h,
                                );
                                players[1].roll = [0.0; 4];
                            }
                        } else if z < sim.terrain.height_at(x, y) {
                            let h = sim.cars[0].heading();
                            sim.cars[0] = SimCar::spawn_facing(
                                &cars[players[0].car].0,
                                &sim.terrain,
                                x as u16,
                                y as u16,
                                h,
                            );
                            players[0].roll = [0.0; 4];
                        }
                        // a changed map is a different track: new time trials, their own best laps
                        for p in players.iter_mut() {
                            p.race = Race::new(best_key(&level, p.car).and_then(|k| load_best(&k)));
                        }
                        ghost = load_ghost(&level, players[0].car);
                        recording = Ghost::default();
                        // a changed map starts a new run (a replay only re-drives the map it came from)
                        pristine = sim.terrain.clone();
                        playing = None;
                        recording_run = new_run(&map, &level, players[0].car, &sim);
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
        if let Some(start) = starting.take() {
            if map != start.map || level.hash != start.map_hash {
                leave = Some(format!(
                    "can't race online: your map {:?} isn't the host's (install the same files)",
                    start.map
                ));
            } else {
                // the same start on both machines: cars, places, time trials, tick 0
                sim.dig_everywhere = start.soft;
                sim.gradual_repair = start.gradual;
                let cam = players[0].cam;
                players = start
                    .cars
                    .iter()
                    .map(|&c| Player::new(c as usize, Race::new(None), cam))
                    .collect();
                sim.cars = players
                    .iter()
                    .zip(start.spawns)
                    .map(|(p, [x, y, h])| {
                        SimCar::spawn_facing(&cars[p.car].0, &sim.terrain, x, y, h)
                    })
                    .collect();
                (ticks, acc) = (0, 0.0);
                playing = None;
                recording = Ghost::default();
                menu_open = false;
                let me = online.as_ref().map_or(0, |(l, _)| l.me);
                gone_seen = vec![false; start.cars.len()];
                banner = Some((
                    format!(
                        "online race on {}, {} cars: you're P{}",
                        start.map,
                        start.cars.len(),
                        me + 1
                    ),
                    get_time() + 4.0,
                ));
            }
        }
        if let Some((link, _)) = &mut online {
            link.poll();
            if let Some(t) = link.desync {
                leave = Some(format!("out of sync with the host (tick {t})"));
            } else if link.lost {
                leave = Some(match link.me {
                    0 => "everyone else left".into(),
                    _ => "the host left".into(),
                });
            } else {
                for (c, seen) in gone_seen.iter_mut().enumerate() {
                    if link.gone[c] && !*seen {
                        *seen = true;
                        banner = Some((
                            format!("P{} left: their car stops", c + 1),
                            get_time() + 4.0,
                        ));
                    }
                }
            }
        }
        if let Some(why) = leave.take() {
            // back to driving alone, in our own car where it stands
            if let Some((link, _)) = online.take() {
                link.bye();
                // ours becomes player 1
                sim.cars.swap(0, link.me);
                players.swap(0, link.me);
                sim.cars.truncate(1);
                players.truncate(1);
                gfx.players = 1;
                gfx.car = players[0].car;
                recording_run = new_run(&map, &level, players[0].car, &sim);
            }
            banner = Some((why, get_time() + 5.0));
        }
        // the Race page looks for hosts on the LAN while it's open
        if menu_open && menu.page == Page::Race && online.is_none() {
            discovery
                .get_or_insert_with(|| net::Discovery::new(net::PORT))
                .poll();
        } else {
            discovery = None;
        }
        let on_join = menu_open
            && items(menu.page, gfx.players).get(menu.selected) == Some(&Item::Row(Row::Join));
        while let Some(c) = get_char_pressed() {
            if on_join && (c.is_ascii_alphanumeric() || ".:-".contains(c)) && gfx.join.len() < 60 {
                gfx.join.push(c);
            }
        }
        if on_join && pressed(KeyCode::Backspace) {
            gfx.join.pop();
        }
        gfx.host_label = match (&online, &lobby) {
            (Some((l, s)), _) => {
                format!("Online: you're P{} on {}  (Enter: leave)", l.me + 1, s.map)
            }
            (None, Lobby::Hosting(h)) if h.guests.is_empty() => format!(
                "Hosting on {}:{}, waiting...  (Enter: stop)",
                net::local_ip().map_or("this machine".into(), |ip| ip.to_string()),
                net::PORT
            ),
            (None, Lobby::Hosting(h)) => format!(
                "Hosting: {} of {} in  (Enter: start, left/right: stop)",
                h.guests.len() + 1,
                net::MAX_PLAYERS
            ),
            _ => "Host an online race".into(),
        };
        let found = discovery.as_ref().map_or(0, |d| d.found.len());
        let cursor = if on_join { "_" } else { "" };
        gfx.join_label = match (&online, &lobby) {
            (Some(_), _) => "Join: (in a race)".into(),
            (None, Lobby::Joining(j)) => match j.lobby {
                Some(n) => format!("In the lobby, {n} in: waiting for the host  (Enter: leave)"),
                None => format!("Joining {}...  (Enter: cancel)", gfx.join),
            },
            _ if found > 0 => format!(
                "Join: {}{cursor}  ({found} on the LAN: left/right)",
                gfx.join
            ),
            _ => format!("Join: {}{cursor}  (type an address)", gfx.join),
        };
        if !menu_open && pressed(KeyCode::Escape) {
            menu_open = true;
            menu = Menu::new();
        } else if menu_open {
            menu.players = gfx.players;
            let enter =
                pressed(KeyCode::Enter) || pressed(KeyCode::KpEnter) || pressed(KeyCode::Space);
            let back = pressed(KeyCode::Escape) || (pressed(KeyCode::Backspace) && !on_join);
            match menu.input(
                pressed(KeyCode::Up),
                pressed(KeyCode::Down),
                pressed(KeyCode::Left),
                pressed(KeyCode::Right),
                enter,
                back,
            ) {
                MenuAction::Close => menu_open = false,
                MenuAction::Quit => break,
                MenuAction::Change(Row::Host, step) => {
                    if online.is_some() {
                        leave = Some("you left the online race".into());
                    } else if let Lobby::Hosting(h) = &lobby {
                        // Enter starts once someone's in; left / right (or Enter alone) stop
                        if step == 0 && !h.guests.is_empty() {
                            (go, menu_open) = (true, false);
                        } else {
                            lobby = Lobby::Off;
                        }
                    } else {
                        match net::Host::bind(net::PORT) {
                            Ok(h) => lobby = Lobby::Hosting(h),
                            Err(e) => banner = Some((format!("can't host: {e}"), get_time() + 5.0)),
                        }
                    }
                }
                MenuAction::Change(Row::Join, 0) => {
                    if online.is_some() {
                        leave = Some("you left the online race".into());
                    } else if let Lobby::Joining(j) = &lobby {
                        j.bye();
                        lobby = Lobby::Off;
                    } else {
                        let addr = if gfx.join.contains(':') {
                            gfx.join.clone()
                        } else {
                            format!("{}:{}", gfx.join, net::PORT)
                        };
                        use std::net::ToSocketAddrs;
                        let joiner = addr
                            .to_socket_addrs()
                            .ok()
                            .and_then(|mut a| a.next())
                            .ok_or(format!("no such address: {addr}"))
                            .and_then(|a| {
                                net::Joiner::new(a, players[0].car as u8, &gfx.skin)
                                    .map_err(|e| e.to_string())
                            });
                        match joiner {
                            Ok(j) => {
                                lobby = Lobby::Joining(j);
                                save_settings(&gfx);
                            }
                            Err(e) => banner = Some((format!("can't join: {e}"), get_time() + 5.0)),
                        }
                    }
                }
                MenuAction::Change(Row::Join, step) => {
                    // step through the hosts found on the LAN
                    if let Some(d) = &discovery
                        && !d.found.is_empty()
                    {
                        let n = d.found.len() as i32;
                        let i = d
                            .found
                            .iter()
                            .position(|(a, _)| a.ip().to_string() == gfx.join);
                        let i = i.map_or(0, |i| (i as i32 + step).rem_euclid(n) as usize);
                        gfx.join = d.found[i].0.ip().to_string();
                    }
                }
                MenuAction::Change(
                    Row::Map | Row::Players | Row::Car | Row::Car2 | Row::Skin | Row::Skin2,
                    _,
                ) if online.is_some() || !matches!(lobby, Lobby::Off) => {
                    // the host sets these when the race starts
                }
                MenuAction::Change(row, step) => {
                    if gfx.change(row, step) {
                        save_settings(&gfx);
                    }
                }
                MenuAction::None => {}
            }
        }
        if gfx.lighting != baked.is_some() {
            // the option was switched (or this is the first frame): bake or drop, re-mesh
            baked = gfx.lighting.then(|| lighting::bake(&sim.terrain));
            meshes = (0..chunks)
                .map(|c| chunk_mesh(&sim.terrain, c, baked.as_ref()))
                .collect();
        }
        let filter = if gfx.nearest {
            FilterMode::Nearest
        } else {
            FilterMode::Linear
        };
        colour_tex.set_filter(filter);
        // each car's skin: the players' choices, or online the ones the race started with
        let skin_files = match &online {
            Some((_, s)) => s.skins.to_vec(),
            None => vec![gfx.skin.clone(), gfx.skin2.clone()],
        };
        skin_files
            .iter()
            .for_each(|f| cache_skin(&mut skins, f, &car_palette));
        cars.iter()
            .map(|(_, skin)| skin)
            .chain(skins.values().flatten())
            .for_each(|skin| skin.texture.set_filter(filter));
        maptex.set_filter(filter);
        if menu_open {
            // paused: no sim ticks, no time banked for later (online the race goes on)
            if online.is_none() {
                acc = 0.0;
            }
        } else if pressed(KeyCode::W) && gfx.players == 1 && !gfx.wasd {
            wireframe = !wireframe;
        } else if pressed(KeyCode::C) {
            gfx.camera = (gfx.camera + 1) % CAMERAS.len();
            save_settings(&gfx);
        }
        let pick = if menu_open || online.is_some() {
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
            .position(|&k| pressed(k))
            .or((gfx.car != players[0].car).then_some(gfx.car))
        };
        if let Some(n) = pick {
            // a new car is a new run: on the map as loaded, its own time trial and best lap
            players[0].car = n;
            if gfx.car != n {
                gfx.car = n;
                save_settings(&gfx);
            }
            playing = None;
            let [x, y, _] = sim.cars[0].centre();
            let h = sim.cars[0].heading();
            if sim.terrain.height != pristine.height || sim.terrain.colour != pristine.colour {
                sim.set_terrain(pristine.clone());
                if let Some(t) = &level.track {
                    sim.set_soft_dirt(t.soft_dirt.clone());
                }
            }
            sim.cars[0] =
                SimCar::spawn_facing(&cars[players[0].car].0, &sim.terrain, x as u16, y as u16, h);
            recording_run = new_run(&map, &level, players[0].car, &sim);
            players[0].race =
                Race::new(best_key(&level, players[0].car).and_then(|k| load_best(&k)));
            ghost = load_ghost(&level, players[0].car);
            recording = Ghost::default();
            players[0].roll = [0.0; 4];
        }
        // the repair option, offline (online the host's holds; a replay keeps its own)
        if online.is_none() && playing.is_none() && sim.gradual_repair != gfx.gradual {
            sim.gradual_repair = gfx.gradual;
            recording_run = new_run(&map, &level, players[0].car, &sim);
        }
        // the car this machine drives: online the joiner's is car 1
        let me = online.as_ref().map_or(0, |(l, _)| l.me);
        // R and F act on the next tick, so a replay reproduces them exactly
        if !menu_open && playing.is_none() {
            players[me].reset |= pressed(KeyCode::R);
            players[me].repair |= pressed(KeyCode::F);
            players[me].tap(pressed(KeyCode::F), sim.gradual_repair);
            players[me].turn |= pressed(KeyCode::T);
        }
        if gfx.players != sim.cars.len() && online.is_none() {
            // player 2 joins beside player 1 (or leaves); either way a new run starts
            if gfx.players == 2 {
                let [x, y, _] = sim.cars[0].centre();
                let h = sim.cars[0].heading();
                let (bx, by) = grid((x as u16, y as u16), h, 1);
                sim.cars.push(SimCar::spawn_facing(
                    &cars[gfx.car2].0,
                    &sim.terrain,
                    bx,
                    by,
                    h,
                ));
                let best = best_key(&level, gfx.car2).and_then(|k| load_best(&k));
                players.push(Player::new(gfx.car2, Race::new(best), players[0].cam));
                banner = Some((
                    "player 2: WASD, Shift brake, Tab back on wheels, G repair, H turn over".into(),
                    get_time() + 4.0,
                ));
            } else {
                sim.cars.truncate(1);
                players.truncate(1);
            }
            playing = None;
            recording_run = new_run(&map, &level, players[0].car, &sim);
        }
        if players.len() == 2 && !menu_open && gfx.car2 != players[1].car && online.is_none() {
            // player 2's new car, where the old one stands
            let [x, y, _] = sim.cars[1].centre();
            let h = sim.cars[1].heading();
            sim.cars[1] =
                SimCar::spawn_facing(&cars[gfx.car2].0, &sim.terrain, x as u16, y as u16, h);
            let best = best_key(&level, gfx.car2).and_then(|k| load_best(&k));
            let cam = players[1].cam;
            players[1] = Player::new(gfx.car2, Race::new(best), cam);
        }
        if players.len() == 2 && !menu_open && online.is_none() {
            players[1].reset |= pressed(KeyCode::Tab);
            players[1].repair |= pressed(KeyCode::G);
            players[1].tap(pressed(KeyCode::G), sim.gradual_repair);
            players[1].turn |= pressed(KeyCode::H);
        }
        // replays are single-player: player 2's inputs aren't in the run
        if !menu_open && pressed(KeyCode::P) && sim.cars.len() == 1 && online.is_none() {
            let name = format!("replays/run-{}.rfr", ticks);
            banner = Some((
                save_replay(&name, &recording_run).map_or("can't save replays here".into(), |p| {
                    format!("saved {}", p.display())
                }),
                get_time() + 3.0,
            ));
        }
        let (model, skin) = (
            &cars[players[0].car].0,
            skin_of(&skins, &cars, players[0].car, &skin_files[0]),
        );
        // player 1's steering keys: the arrows, or WASD alone (the pad still sends arrows)
        let wasd = gfx.wasd && gfx.players == 1;
        let steer = |arrow: KeyCode, letter: KeyCode| match wasd {
            true => is_key_down(letter) || pad_held.contains(&arrow),
            false => down(arrow),
        };
        let keys = Keys {
            throttle: steer(KeyCode::Up, KeyCode::W) || shot.is_some(),
            reverse: steer(KeyCode::Down, KeyCode::S),
            left: steer(KeyCode::Left, KeyCode::A)
                || (shot.is_some() && get_time() - started > 2.0),
            right: steer(KeyCode::Right, KeyCode::D),
            brake: down(KeyCode::Space) || down(KeyCode::Kp5),
        };
        let keys2 = Keys {
            throttle: down(KeyCode::W),
            reverse: down(KeyCode::S),
            left: down(KeyCode::A),
            right: down(KeyCode::D),
            brake: down(KeyCode::LeftShift),
        };
        // Fixed 120 Hz sim like the original timer interrupt; drawing runs free.
        if !menu_open || online.is_some() {
            acc += get_frame_time().min(0.25);
        }
        while acc >= 1.0 / TICK_HZ as f32 {
            // online: our keys go DELAY ticks ahead; a tick runs once the other side's are in
            let net_inputs = match &mut online {
                Some((link, _)) => {
                    if link.local_ticks() <= ticks as usize + net::DELAY {
                        let k = if menu_open { Keys::default() } else { keys };
                        let p = &mut players[link.me];
                        let mend = sim.gradual_repair && !menu_open && down(KeyCode::F);
                        link.push_local(run::input(
                            k,
                            p.reset,
                            p.repair || mend || p.full,
                            p.turn || p.full,
                        ));
                        (p.reset, p.repair, p.turn, p.full) = (false, false, false, false);
                    }
                    match link.inputs(ticks as usize) {
                        Some(i) => Some(i),
                        None => {
                            acc = acc.min(0.1); // waiting on the other player
                            break;
                        }
                    }
                }
                None => None,
            };
            acc -= 1.0 / TICK_HZ as f32;
            ticks += 1;
            let byte = match (&net_inputs, &mut playing) {
                (Some(i), _) => i[0],
                (None, Some((r, i))) => {
                    *i += 1;
                    r.inputs[*i - 1]
                }
                (None, None) => run::input(
                    keys,
                    players[0].reset,
                    players[0].repair
                        || players[0].full
                        || (sim.gradual_repair && !menu_open && down(KeyCode::F)),
                    players[0].turn || players[0].full,
                ),
            };
            if playing.as_ref().is_some_and(|(r, i)| *i >= r.inputs.len()) {
                playing = None;
                banner = Some(("replay over: you're driving".into(), get_time() + 3.0));
            }
            // every car's input: player 1's above; the others' from the link online, else player 2
            // on WASD (online, R / F wait for the next input sent, which may be a tick or two away)
            let mut inputs = vec![byte];
            for (i, p) in players.iter_mut().enumerate().skip(1) {
                inputs.push(match &net_inputs {
                    Some(n) => n[i],
                    None => run::input(
                        keys2,
                        p.reset,
                        p.repair
                            || p.full
                            || (sim.gradual_repair && !menu_open && down(KeyCode::G)),
                        p.turn || p.full,
                    ),
                });
            }
            if online.is_none() {
                for p in players.iter_mut() {
                    (p.reset, p.repair, p.turn, p.full) = (false, false, false, false);
                }
                recording_run.inputs.push(byte);
            }
            let mut races: Vec<Race> = players
                .iter_mut()
                .map(|p| std::mem::replace(&mut p.race, Race::new(None)))
                .collect();
            let models: Vec<&Car> = players.iter().map(|p| &cars[p.car].0).collect();
            let outs = run::tick_all(
                &mut sim,
                &models,
                level.track.as_ref(),
                &mut races,
                &inputs,
                ticks,
            );
            for (p, r) in players.iter_mut().zip(races) {
                p.race = r;
            }
            if byte & run::REPAIR != 0 && !sim.gradual_repair {
                banner = Some(("repaired".into(), get_time() + 1.0));
            }
            // each car's wheels and time trial; player 1 also keeps the ghost and best-lap replay.
            // Banners for this machine's player go plain, the others' with "P2: " etc; the
            // others first, so ours wins a tie
            let solo = players.len() == 1;
            for (i, o) in outs.iter().enumerate().rev() {
                let p = &mut players[i];
                if o.respawned {
                    p.roll = [0.0; 4];
                }
                let spin = sim.cars[i].spin();
                for w in 0..4 {
                    // spin is rim travel per tick (16.16 world units)
                    p.roll[w] += spin[w] as f32 / 65536.0 / cars[p.car].0.atoms[w].radius;
                }
                let (Some(t), Some(ev)) = (&level.track, &o.event) else {
                    continue;
                };
                if let Some(a) = &audio {
                    a.chime(&o.event, gfx.volume as f32 / 10.0);
                }
                let who = if i == me {
                    String::new()
                } else {
                    format!("P{}: ", i + 1)
                };
                let n = t.gates.len();
                let msg = match *ev {
                    Event::Start => {
                        if i == 0 {
                            recording = Ghost::default();
                        }
                        format!("{who}GO!")
                    }
                    Event::Gate(g) => match p.race.split_delta() {
                        Some(d) => format!(
                            "{who}checkpoint {g}/{}   {}{}",
                            n - 1,
                            if d < 0 { "-" } else { "+" },
                            format_time(d.unsigned_abs())
                        ),
                        None => format!("{who}checkpoint {g}/{}", n - 1),
                    },
                    Event::Lap { time, best } => {
                        let lap = if i == 0 {
                            std::mem::take(&mut recording)
                        } else {
                            Ghost::default()
                        };
                        if best
                            && online.is_none()
                            && let (Some(k), Some(b)) = (best_key(&level, p.car), &p.race.best)
                        {
                            save_best(&k, b);
                            if i == 0 {
                                save_ghost(&k, &lap);
                                if solo {
                                    save_replay(&format!("replay-{k}.rfr"), &recording_run);
                                }
                                ghost = lap;
                            }
                        }
                        format!(
                            "{who}lap {}{}",
                            format_time(time),
                            if best { "   best!" } else { "" }
                        )
                    }
                };
                banner = Some((msg, get_time() + 2.5));
            }
            if let Some((link, _)) = &mut online {
                link.ran(ticks, || sim.state_hash());
            }
            let race = &players[0].race;
            if race.started && race.lap_time(ticks).is_multiple_of(ghost::STEP) && online.is_none()
            {
                recording.frames.push(Frame {
                    atoms: sim.cars[0].atom_positions(),
                    steer: sim.cars[0].steer_angle(),
                    roll: players[0].roll,
                });
            }
        }
        if let Some(a) = &mut audio {
            let vol = if menu_open {
                0.0
            } else {
                gfx.volume as f32 / 10.0
            };
            // the original's engine (0x02cf): pitch from the driven wheels' spin, loudness from
            // the throttle control word; one per car in split screen
            // ours and the first other car
            let me = online.as_ref().map_or(0, |(l, _)| l.me);
            let heard = [
                me,
                (0..sim.cars.len()).find(|&c| c != me).unwrap_or(usize::MAX),
            ];
            for (p, &c) in heard.iter().enumerate() {
                let (fnum, gain) = match sim.cars.get(c) {
                    Some(c) => (
                        sound::engine_fnum(c.revs()) as f32,
                        sound::engine_gain(c.throttle_word()),
                    ),
                    None => (48.0, 0.0),
                };
                // wheel spin flickers tick to tick: smooth it (~0.1 s) so the tone doesn't hop
                revs_smooth[p] += (fnum - revs_smooth[p]) * (get_frame_time() * 10.0).min(1.0);
                a.engine(
                    p,
                    gfx.engine,
                    revs_smooth[p],
                    vol * ENGINE_LEVEL * gain,
                    get_frame_time(),
                );
            }
            // as loud as the slide particles in the air; squeal for those off hard ground
            let (mut soft, mut hard) = (0.0f32, 0.0f32);
            for c in sim.particle_colours() {
                let rgb = sim.terrain.palette[c as usize];
                if c != 0x0f && hard_ground(rgb) {
                    hard += 1.0;
                } else {
                    soft += 1.0;
                }
            }
            let total = ((soft + hard) / SKID_FULL).min(1.0);
            let share = |n: f32| {
                if soft + hard > 0.0 {
                    total * n / (soft + hard)
                } else {
                    0.0
                }
            };
            for (k, n) in [soft, hard].into_iter().enumerate() {
                skid[k] += (share(n) - skid[k]) * 0.2;
            }
            a.skid(skid, vol);
            if let Some(r) = &mut audio_rec {
                r.frame(
                    a,
                    gfx.engine,
                    skid.map(|s| vol * SKID_LEVEL * s),
                    get_frame_time(),
                );
            }
            if sim.impacts > impacts && get_time() - last_thud > 0.15 && vol > 0.0 {
                play_sound(
                    &a.thud,
                    PlaySoundParams {
                        looped: false,
                        volume: vol * THUD_LEVEL,
                    },
                );
                if let Some(r) = &mut audio_rec {
                    r.thuds.push((0, vol * THUD_LEVEL));
                }
                last_thud = get_time();
            }
        }
        impacts = sim.impacts;
        // the car this machine drives: online the joiner's is car 1
        let me = online.as_ref().map_or(0, |(l, _)| l.me);
        let c = sim.cars[me].centre();
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
            meshes[c] = chunk_mesh(&sim.terrain, c, baked.as_ref());
        }
        shown.copy_from_slice(&sim.terrain.height);
        if shown_colour != sim.terrain.colour {
            // landing dirt darkens COL cells
            colour_tex.update(&colour_image(&sim.terrain));
            index_tex.update(&index_image(&sim.terrain));
            shown_colour.copy_from_slice(&sim.terrain.colour);
        }

        let atoms = sim.cars[0].atom_positions();
        let dt = get_frame_time();
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
        orbit += (down(KeyCode::E) as i32 - down(KeyCode::Q) as i32) as f32 * dt * 2.0;
        // views set their own cameras below; this one only points clear_background at the target
        set_camera(&Camera3D {
            render_target: target.clone(),
            ..Default::default()
        });
        // fog off: push it past anything on screen
        let range = if gfx.fog { FOG_RANGE } else { [1e6, 1e6 + 1.0] };
        let [r, g, b] = sim.terrain.palette[255];
        for m in [&fog, &terrain_material, &object_material] {
            m.set_uniform("FogColor", [r, g, b].map(|c| c as f32 / 255.0));
            m.set_uniform("FogRange", range);
        }
        clear_background(Color::from_rgba(r, g, b, 255));
        // one view per player: side by side in two-player mode
        let dpi = screen_dpi_scale();
        let (fw, fh) = (screen_width() * dpi, screen_height() * dpi);
        // split screen shows every car; online, just ours
        let views: Vec<usize> = if online.is_some() {
            vec![me]
        } else {
            (0..sim.cars.len()).collect()
        };
        let split = views.len() > 1;
        for (slot, &view) in views.iter().enumerate() {
            let (position, look_at, up) = {
                let p = &mut players[view];
                follow_camera(&sim, view, gfx.camera, orbit, &mut p.cam, &mut p.arm, dt)
            };
            let w = if split { fw / 2.0 } else { fw };
            set_camera(&Camera3D {
                position,
                target: look_at,
                up,
                render_target: target.clone(),
                aspect: split.then_some(w / fh),
                viewport: split.then_some(((slot as f32 * w) as i32, 0, w as i32, fh as i32)),
                ..Default::default()
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
            gl_use_material(&object_material);
            for m in object_meshes(&level, &sim.terrain, &objtex, &sprites, position) {
                draw_mesh(&m);
            }
            gl_use_material(&fog);
            for (ci, c) in sim.cars.iter().enumerate() {
                // bumper camera sits inside the viewer's body: don't draw it
                let own = if gfx.camera == 1 && ci == view { 0 } else { 3 };
                let car = players[ci].car;
                let file = &skin_files[ci.min(skin_files.len() - 1)];
                let (m, sk) = (&cars[car].0, skin_of(&skins, &cars, car, file));
                for mesh in car_meshes(
                    m,
                    &c.atom_positions(),
                    c.steer_angle(),
                    &car_palette,
                    &|x, y| sim.terrain.height_at(x, y),
                    sk,
                    position,
                    up,
                    gfx.textured,
                )
                .iter()
                .take(own)
                {
                    draw_mesh(mesh);
                }
            }
            // the best lap's ghost, in step with the lap timer
            if gfx.ghost
                && view == 0
                && players[0].race.started
                && let Some(f) = ghost.at(players[0].race.lap_time(ticks))
            {
                for mut m in car_meshes(
                    model,
                    &f.atoms,
                    f.steer,
                    &car_palette,
                    &|x, y| sim.terrain.height_at(x, y),
                    skin,
                    position,
                    up,
                    gfx.textured,
                ) {
                    m.vertices
                        .iter_mut()
                        .for_each(|v| v.color[3] = (v.color[3] as u16 * 2 / 5) as u8);
                    draw_mesh(&m);
                }
            }
            if wireframe && view == 0 {
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
            // gates: an arch (two posts and a banner) over each painted line, and the arrow to the
            // next (a CHECK.PCX may have only a spawn, or zones: no gates, nothing to point at)
            if let Some(t) = level.track.as_ref().filter(|t| !t.gates.is_empty()) {
                // the bright next gate first: translucent ones after it, so they don't hide it
                let next = players[view].race.next;
                let order = std::iter::once(next).chain((0..t.gates.len()).filter(|&g| g != next));
                for g in order {
                    for piece in &t.gates[g] {
                        let top = piece
                            .cells
                            .iter()
                            .map(|&(x, y)| {
                                sim.terrain
                                    .height_at((x as f32 + 0.5) * S, (y as f32 + 0.5) * S)
                            })
                            .fold(f32::MIN, f32::max);
                        draw_gate(
                            &sim.terrain,
                            piece.ends,
                            top + GATE_HEIGHT * S,
                            g == 0,
                            g == next,
                            position,
                        );
                    }
                }
                if gfx.guide && gfx.camera != 1 {
                    // (not from the bumper camera: it would hover above the screen's top edge)
                    // forks: the nearest piece of the next gate
                    let [cx, cy, _] = sim.cars[view].centre();
                    draw_guide(&sim.cars[view], t.piece(next, [cx, cy]).centre, get_time());
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
        let help = match (&online, sim.cars.len()) {
            (Some(_), _) => format!("online  P{}", me + 1),
            (None, 1) => "esc: menu (controls are listed there)   |  ".into(),
            _ => "P1".into(),
        };
        let hud = format!(
            "{help} car {}  {:3.0} km/h   damage {:2.0}%{}   ruts {dents}{}",
            players[me].car + 1,
            speed * 0.019 * 3.6, // ~210 world units per 4 m car
            sim.cars[me].damage() * 100.0,
            if sim.gradual_repair {
                format!(
                    "   repair {:3.0}%",
                    sim.cars[me].budget() as f32 / REPAIR_CAP as f32 * 100.0
                )
            } else {
                String::new()
            },
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
        text_px(&hud, 12.0, 22.0, 18.0, WHITE);
        // the driving keys, bottom left, for the first half minute, then fading out
        let shown = 1.0 - (ticks as f32 / TICK_HZ as f32 - 25.0) / 5.0;
        if online.is_none() && sim.cars.len() == 1 && shown > 0.0 {
            let a = shown.min(1.0);
            let first = if gfx.wasd {
                "WASD / left stick: drive"
            } else {
                "Arrows / left stick: drive"
            };
            let lines = &[[first].as_slice(), &CONTROLS[1..5]].concat();
            let (x, y) = (12.0, screen_height() - 16.0 - lines.len() as f32 * 24.0);
            let width = lines
                .iter()
                .map(|l| measure_text(l, None, 18, 1.0).width)
                .fold(0.0, f32::max);
            draw_rectangle(
                x - 8.0,
                y - 8.0,
                width + 16.0,
                lines.len() as f32 * 24.0 + 16.0,
                Color::new(0.0, 0.0, 0.0, 0.45 * a),
            );
            for (i, l) in lines.iter().enumerate() {
                text_px(
                    l,
                    x,
                    y + 16.0 + i as f32 * 24.0,
                    18.0,
                    Color::new(1.0, 1.0, 1.0, a),
                );
            }
        }
        if let Some(t) = &level.track {
            let n = t.gates.len();
            let line = |race: &Race, place: &str| {
                if race.started {
                    let passed = (race.next + n - 1) % n;
                    let opt = |l: Option<u64>| l.map_or("-:--.--".into(), format_time);
                    format!(
                        "{place}lap {}   {}   checkpoints {passed}/{}   last {}   best {}",
                        race.laps + 1,
                        format_time(race.lap_time(ticks)),
                        n - 1,
                        opt(race.last_lap),
                        opt(race.best.as_ref().map(|b| b.time))
                    )
                } else {
                    format!(
                        "{place}time trial: drive through the start line   best {}",
                        race.best
                            .as_ref()
                            .map_or("-:--.--".into(), |b| format_time(b.time))
                    )
                }
            };
            if players.len() > 1 {
                // placings: further round the track first, the lower number on a tie
                let progress: Vec<f32> = (0..players.len())
                    .map(|i| {
                        let [x, y, _] = sim.cars[i].centre();
                        players[i].race.progress(t, [x, y])
                    })
                    .collect();
                let size = if screen_width() < 1400.0 { 20.0 } else { 26.0 };
                let column = screen_width() / players.len() as f32;
                for (i, p) in players.iter().enumerate() {
                    let ahead = (0..players.len())
                        .filter(|&j| {
                            progress[j] > progress[i] || (progress[j] == progress[i] && j < i)
                        })
                        .count();
                    let place = ["1st", "2nd", "3rd", "4th"]
                        .get(ahead)
                        .copied()
                        .unwrap_or("");
                    let colour = [YELLOW, SKYBLUE, PINK, ORANGE][i % 4];
                    // three or four columns: just the place, lap, time and checkpoints
                    let text = if players.len() > 2 {
                        let r = &p.race;
                        match r.started {
                            true => format!(
                                "P{} {place}  lap {}  {}  {}/{}",
                                i + 1,
                                r.laps + 1,
                                format_time(r.lap_time(ticks)),
                                (r.next + n - 1) % n,
                                n - 1
                            ),
                            false => format!("P{} {place}  to the start", i + 1),
                        }
                    } else {
                        line(&p.race, &format!("P{} {place}  ", i + 1))
                    };
                    text_px(text, i as f32 * column + 12.0, 52.0, size, colour);
                }
                if split {
                    draw_line(
                        screen_width() / 2.0,
                        0.0,
                        screen_width() / 2.0,
                        screen_height(),
                        2.0,
                        BLACK,
                    );
                }
            } else {
                text_px(line(&players[0].race, ""), 12.0, 52.0, 30.0, YELLOW);
            }
        }
        if let Some((r, i)) = &playing {
            let msg = format!(
                "REPLAY  {} / {}",
                format_time(*i as u64),
                format_time(r.inputs.len() as u64)
            );
            let size = measure_text(&msg, None, 30, 1.0);
            text_px(
                &msg,
                (screen_width() - size.width) / 2.0,
                band + 36.0,
                30.0,
                RED,
            );
        }
        if gfx.minimap && !menu_open {
            // ghost dot: midpoint of the axle atoms 4 and 6, like `SimCar::centre`
            let race = &players[0].race;
            let ghost_at = (gfx.ghost && race.started)
                .then(|| ghost.at(race.lap_time(ticks)))
                .flatten()
                .map(|f| {
                    [
                        (f.atoms[4][0] + f.atoms[6][0]) / 2.0,
                        (f.atoms[4][1] + f.atoms[6][1]) / 2.0,
                    ]
                });
            let others: Vec<&SimCar> = (sim.cars.iter().enumerate())
                .filter(|&(i, _)| i != me)
                .map(|(_, c)| c)
                .collect();
            draw_minimap(
                &colour_tex,
                &level.zone_cells,
                level.track.as_ref(),
                players[me].race.next,
                &sim.cars[me],
                &others,
                ghost_at,
                band,
            );
        }
        if let Some((msg, until)) = &banner
            && get_time() < *until
            && !menu_open
        {
            let size = measure_text(msg, None, 48, 1.0);
            text_px(
                msg,
                (screen_width() - size.width) / 2.0,
                screen_height() * 0.2,
                48.0,
                YELLOW,
            );
        }
        if split && sim.cars[1].upside_down() && !menu_open {
            let msg = "P2 upside down - Tab";
            let size = measure_text(msg, None, 36, 1.0);
            text_px(
                msg,
                screen_width() * 0.75 - size.width / 2.0,
                screen_height() * 0.3,
                36.0,
                SKYBLUE,
            );
        }
        if sim.cars[me].upside_down() && !menu_open {
            let msg = "upside down - press R";
            let size = measure_text(msg, None, 40, 1.0);
            text_px(
                msg,
                (screen_width() - size.width) / 2.0,
                screen_height() * 0.3,
                40.0,
                YELLOW,
            );
        }
        if menu_open {
            draw_menu(&gfx, &mut menu, get_frame_time());
            match items(menu.page, gfx.players).get(menu.selected) {
                Some(Item::Row(Row::Map)) => {
                    let preview = map_previews.entry(gfx.map.clone()).or_insert_with(|| {
                        let (terrain, level) = load_level(&gfx.map).ok()?;
                        let tex = Texture2D::from_image(&colour_image(&terrain));
                        tex.set_filter(FilterMode::Nearest);
                        Some((tex, level))
                    });
                    if let Some((tex, level)) = preview {
                        draw_map_preview(&gfx.map, tex, level);
                    }
                }
                Some(&Item::Row(row @ (Row::Car | Row::Skin | Row::Car2 | Row::Skin2))) => {
                    let (n, file) = if matches!(row, Row::Car | Row::Skin) {
                        (gfx.car, &gfx.skin)
                    } else {
                        (gfx.car2, &gfx.skin2)
                    };
                    let (model, skin) = (&cars[n].0, skin_of(&skins, &cars, n, file));
                    let drive = match model.drive {
                        car::Drive::Rear => "rear-wheel drive",
                        car::Drive::Front => "front-wheel drive",
                        car::Drive::All => "four-wheel drive",
                    };
                    draw_car_preview(
                        &car_target,
                        &format!("car {}  ({drive})  {}", n + 1, skin_name(file)),
                        model,
                        skin,
                        &car_palette,
                        gfx.textured,
                        get_time() as f32 * 0.7,
                    );
                }
                _ => {}
            }
        }
        // F12: the frame as shown into the data folder; the banner saying so starts next frame,
        // so it's not in the picture
        #[cfg(not(target_arch = "wasm32"))]
        if is_key_pressed(KeyCode::F12) {
            banner = Some(match save_screenshot() {
                Ok(p) => (format!("saved {}", p.display()), get_time() + 3.0),
                Err(e) => (format!("can't save the screenshot: {e}"), get_time() + 5.0),
            });
        }
        if let Some(path) = &shot {
            if get_time() - started > 4.0 {
                get_screen_data().export_png(path);
                break;
            }
        }
        next_frame().await;
    }
    if let Some(r) = &mut audio_rec {
        r.save();
    }
    if let Some((link, _)) = &online {
        link.bye();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_names() {
        assert_eq!(timestamp(0), "1970-01-01-000000");
        assert_eq!(timestamp(951_782_400), "2000-02-29-000000"); // a leap day
        assert_eq!(timestamp(1_792_489_530), "2026-10-20-094530");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn pad_maps_to_keys() {
        let none = |_| false;
        assert!(
            pad_down(0.2, 0.0, 0.0, none).is_empty(),
            "inside the deadzone"
        );
        assert_eq!(pad_down(-0.9, 0.8, 0.0, none), [KeyCode::Left, KeyCode::Up]);
        assert_eq!(
            pad_down(0.0, 0.0, 0.0, |b| b == Button::East),
            [KeyCode::Space]
        );
        assert_eq!(pad_pressed(Button::Start), Some(KeyCode::Escape));
        assert_eq!(pad_pressed(Button::Mode), None);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn inputs_resting_deflected_wait_for_rest() {
        // a Thrustmaster HOTAS: both triggers rest at 1.0 and the stick axis at 0
        let mut r = Rested::default();
        assert_eq!(
            r.filter([0.0, 1.0, 1.0]),
            [0.0, 0.0, 0.0],
            "not seen at rest yet"
        );
        assert_eq!(
            r.filter([-0.9, 1.0, 1.0]),
            [-0.9, 0.0, 0.0],
            "the stick rested"
        );
        // released once, the right trigger counts from then on
        assert_eq!(r.filter([0.0, 0.1, 1.0]), [0.0, 0.1, 0.0]);
        assert_eq!(r.filter([0.0, 0.8, 1.0]), [0.0, 0.8, 0.0]);
    }

    #[test]
    fn menu_navigation() {
        let mut m = Menu::new();
        let none = |m: &mut Menu, up, down, left, right, enter, back| {
            m.input(up, down, left, right, enter, back)
        };
        // down to Graphics and open it
        none(&mut m, false, true, false, false, false, false);
        none(&mut m, false, true, false, false, false, false);
        assert_eq!(items(Page::Top, 1)[m.selected], Item::Open(Page::Graphics));
        assert_eq!(
            none(&mut m, false, false, false, false, true, false),
            MenuAction::None
        );
        assert_eq!(m.page, Page::Graphics);
        // left/right/enter on a setting change it
        assert_eq!(
            none(&mut m, false, false, false, true, false, false),
            MenuAction::Change(Row::Textures, 1)
        );
        assert_eq!(
            none(&mut m, false, false, true, false, false, false),
            MenuAction::Change(Row::Textures, -1)
        );
        assert_eq!(
            none(&mut m, false, false, false, false, true, false),
            MenuAction::Change(Row::Textures, 0)
        );
        // clamped at the ends, no wrap
        for _ in 0..20 {
            none(&mut m, false, true, false, false, false, false);
        }
        assert_eq!(
            items(Page::Graphics, 1)[m.selected],
            Item::Row(Row::Resolution)
        );
        // Esc goes back to the top, on the row we came from; Esc again closes
        assert_eq!(
            none(&mut m, false, false, false, false, false, true),
            MenuAction::None
        );
        assert_eq!(
            (m.page, items(Page::Top, 1)[m.selected]),
            (Page::Top, Item::Open(Page::Graphics))
        );
        assert_eq!(
            none(&mut m, false, false, false, false, false, true),
            MenuAction::Close
        );
        // ← on a read-only line goes back too (Controls opens on its steering-keys row); Quit quits
        let mut m = Menu::new();
        m.selected = 4;
        none(&mut m, false, false, false, true, false, false);
        assert_eq!(m.page, Page::Controls);
        assert_eq!(
            none(&mut m, false, false, true, false, false, false),
            MenuAction::Change(Row::Keys, -1)
        );
        none(&mut m, false, true, false, false, false, false);
        none(&mut m, false, false, true, false, false, false);
        assert_eq!(m.page, Page::Top);
        m.selected = 5;
        assert_eq!(
            none(&mut m, false, false, false, false, true, false),
            MenuAction::Quit
        );
    }

    #[test]
    fn picks_maps() {
        reformers::need_assets!();
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
            car: 4,
            car2: 2,
            skin: "GHOST.PCX".into(),
            skin2: String::new(),
            join: "192.168.1.20".into(),
            guide: true,
            gradual: true,
            wasd: true,
            host_label: String::new(),
            join_label: String::new(),
            players: 1,
            camera: 2,
            nearest: false,
            textured: true,
            lighting: true,
            crt: true,
            minimap: false,
            ghost: true,
            fog: false,
            volume: 3,
            engine: 1,
            fullscreen: false,
            resolution: 3,
        };
        let mut b = Graphics {
            map: "x".into(),
            car: 0,
            car2: 1,
            skin: "X.PCX".into(),
            skin2: "Y.PCX".into(),
            join: "192.168.1.20".into(),
            guide: false,
            gradual: false,
            wasd: false,
            host_label: String::new(),
            join_label: String::new(),
            players: 1,
            camera: 0,
            nearest: true,
            textured: false,
            lighting: false,
            crt: false,
            minimap: true,
            ghost: false,
            fog: true,
            volume: 10,
            engine: 0,
            fullscreen: true,
            resolution: 0,
        };
        b.apply_text(&a.to_text());
        assert_eq!(b.to_text(), a.to_text());
        a.apply_text("camera=99\nresolution=x\ncar=0\ncar2=6\nbogus=true\nno equals sign");
        assert_eq!(
            (a.camera, a.resolution, a.car, a.car2),
            (2, 3, 4, 2),
            "bad values are skipped"
        );
    }

    #[test]
    fn player_two_car_row_only_with_two_players() {
        let race = |players| items(Page::Race, players);
        assert!(!race(1).contains(&Item::Row(Row::Car2)));
        let two = race(2);
        let at = |r| two.iter().position(|&i| i == Item::Row(r)).unwrap();
        // listed after Players, so toggling Players doesn't move the selection
        assert!(at(Row::Players) < at(Row::Car2));
        assert!(at(Row::Car2) < at(Row::Skin2));
    }

    /// Every installed map (the community pack's 320-wide and 258-square images included) loads,
    /// and a car spawned the way the game does it drives on it without the sim blowing up.
    #[test]
    fn every_map_loads_and_drives() {
        reformers::need_assets!();
        let car = car::parse(&assets::read("CAR1.DAT").unwrap()).unwrap();
        let maps = assets::maps();
        assert!(
            maps.len() > 100,
            "the pack's maps are installed: {}",
            maps.len()
        );
        for name in maps {
            let (terrain, level) = load_level(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
            let ((x, y), h) = level
                .track
                .as_ref()
                .and_then(|t| t.spawn.as_ref())
                .map_or((SPAWN, 0), |s| ((s.x, s.y), s.heading));
            let mut sim = Sim::new(terrain);
            sim.cars
                .push(SimCar::spawn_facing(&car, &sim.terrain, x, y, h));
            let gas = Keys {
                throttle: true,
                ..Keys::default()
            };
            for _ in 0..TICK_HZ * 3 {
                sim.tick(&[gas]);
            }
            let [cx, cy, cz] = sim.cars[0].centre();
            assert!(
                [cx, cy, cz].iter().all(|v| v.is_finite())
                    && cz > sim.terrain.height_at(cx, cy) - 512.0,
                "{name}: car at {cx} {cy} {cz}"
            );
        }
    }

    #[test]
    fn skins_cycle_through_the_cars_own() {
        reformers::need_assets!();
        let last = skin_list().last().unwrap().0.clone();
        assert_eq!(next_skin("", -1), last);
        assert_eq!(next_skin(&last, 1), "");
        assert_eq!(next_skin("", 0), skin_list()[0].0, "Enter steps forward");
        assert_eq!(next_skin("GONE.PCX", 1), skin_list()[0].0);
        assert_eq!(skin_name("HUFO.PCX"), "ufo (hover)");
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
