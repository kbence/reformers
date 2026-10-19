//! `editor [MAP] [--shot FILE]`: the Reformers map editor (#36). This first version edits a map's checkpoint
//! layer, CHECK.PCX (format in `track.rs`): the spawn, the gates in lap order, and the zones
//! (reset, boost, slow, soft dirt). It saves into the map's folder, where a running game picks
//! the file up within a second (hot reload), so edit here and drive there.
//!
//! The layer is 256×256 cells, one per terrain cell: index >> 4 = element, & 15 = setting.
//! Every change is checked by loading it the way the game does (`Track::load`).

use macroquad::prelude::*;
use reformers::terrain::{SIZE, Terrain};
use reformers::track::{BOOST, GATE, RESET, SLOW, SOFT, SPAWN, Track};
use reformers::{assets, pcx};

/// The checkpoint layer: one index per terrain cell.
#[derive(Clone, PartialEq, Debug)]
struct Layer(Vec<u8>);

impl Layer {
    fn get(&self, x: usize, y: usize) -> u8 {
        self.0[y * SIZE + x]
    }

    fn set(&mut self, x: i32, y: i32, v: u8) {
        if (0..SIZE as i32).contains(&x) && (0..SIZE as i32).contains(&y) {
            self.0[y as usize * SIZE + x as usize] = v;
        }
    }

    /// The one spawn: a 3×3 blob at (x, y) facing `dir` (0–3: up, right, down, left on the map).
    fn set_spawn(&mut self, x: i32, y: i32, dir: u8) {
        self.0
            .iter_mut()
            .filter(|v| **v >> 4 == SPAWN)
            .for_each(|v| *v = 0);
        for (dx, dy) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))) {
            self.set(x + dx, y + dy, SPAWN << 4 | dir);
        }
    }

    /// The spawn's centre cell and direction, if there is one.
    fn spawn(&self) -> Option<(f32, f32, u8)> {
        let cells: Vec<(usize, u8)> = (0..SIZE * SIZE)
            .filter(|&i| self.0[i] >> 4 == SPAWN)
            .map(|i| (i, self.0[i] & 15))
            .collect();
        let n = cells.len() as f32;
        let (sx, sy) = cells.iter().fold((0.0, 0.0), |(x, y), &(i, _)| {
            (x + (i % SIZE) as f32, y + (i / SIZE) as f32)
        });
        Some((sx / n, sy / n, cells.first()?.1))
    }

    /// Gate `n` (setting 0–15, lap order) as a line of cells from `a` to `b`.
    fn gate_line(&mut self, a: (i32, i32), b: (i32, i32), n: u8) {
        let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).max(1);
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let x = a.0 as f32 + (b.0 - a.0) as f32 * t;
            let y = a.1 as f32 + (b.1 - a.1) as f32 * t;
            self.set(x.round() as i32, y.round() as i32, GATE << 4 | n);
        }
    }

    /// The gate settings in use, ascending (= lap order; the first is start/finish).
    fn gates(&self) -> Vec<u8> {
        let mut used = [false; 16];
        self.0
            .iter()
            .filter(|v| **v >> 4 == GATE)
            .for_each(|v| used[(v & 15) as usize] = true);
        (0..16).filter(|&n| used[n as usize]).collect()
    }

    /// The setting a new gate gets: after the last one, or the first gap when 15 is taken.
    fn next_gate(&self) -> Option<u8> {
        let used = self.gates();
        match used.last() {
            None => Some(0),
            Some(&last) if last < 15 => Some(last + 1),
            _ => (0..16).find(|n| !used.contains(n)),
        }
    }

    fn erase_gate(&mut self, n: u8) {
        self.0
            .iter_mut()
            .filter(|v| **v == GATE << 4 | n)
            .for_each(|v| *v = 0);
    }

    /// Paints a disc of radius `r` cells with `v` (0 erases).
    fn brush(&mut self, x: i32, y: i32, r: i32, v: u8) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r + r {
                    self.set(x + dx, y + dy, v);
                }
            }
        }
    }

    /// As CHECK.PCX: the 320-wide layout of the originals (blank legend strip), colours per
    /// element so it reads in a paint program too.
    fn encode(&self) -> Vec<u8> {
        let pixels = self
            .0
            .chunks_exact(SIZE)
            .flat_map(|r| [r, &[0; 64]].concat())
            .collect();
        pcx::encode(&pcx::Image {
            width: 320,
            height: SIZE,
            pixels,
            palette: std::array::from_fn(|i| {
                let c = colour(i as u8);
                [c.r, c.g, c.b].map(|v| (v * 255.0) as u8)
            }),
        })
    }
}

/// How an element looks (also the saved palette); brighter settings for stronger zones.
fn colour(v: u8) -> Color {
    let shade = 0.55 + 0.45 * (v & 15) as f32 / 15.0;
    let [r, g, b] = match v >> 4 {
        SPAWN => [1.0, 1.0, 1.0],
        GATE => [1.0, 0.85, 0.1],
        RESET => [0.9, 0.1, 0.1],
        BOOST => [0.2, 0.9, 1.0],
        SLOW => [0.55, 0.35, 0.12],
        SOFT => [0.95, 0.55, 0.15],
        _ => return Color::new(0.0, 0.0, 0.0, 0.0),
    };
    let s = if v >> 4 == SPAWN || v >> 4 == GATE {
        1.0
    } else {
        shade
    };
    Color::new(r * s, g * s, b * s, 0.85)
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Spawn,
    Gate,
    Zone(u8),
    Erase,
}

const TOOLS: [(Tool, &str); 7] = [
    (Tool::Spawn, "spawn"),
    (Tool::Gate, "gate"),
    (Tool::Zone(RESET), "reset zone"),
    (Tool::Zone(BOOST), "boost pad"),
    (Tool::Zone(SLOW), "slow zone"),
    (Tool::Zone(SOFT), "soft dirt"),
    (Tool::Erase, "eraser"),
];

fn window_conf() -> Conf {
    Conf {
        window_title: "Reformers map editor".into(),
        window_width: 1100,
        window_height: 900,
        high_dpi: true,
        ..Default::default()
    }
}

/// The map as seen from above: COL colours, shaded by slope like the game's flat lighting.
fn map_texture(t: &Terrain) -> Texture2D {
    let h = |x: usize, y: usize| t.height[y.min(SIZE - 1) * SIZE + x.min(SIZE - 1)] as f32;
    let bytes: Vec<u8> = (0..SIZE * SIZE)
        .flat_map(|i| {
            let (x, y) = (i % SIZE, i / SIZE);
            let slope = (h(x.saturating_sub(1), y) - h(x + 1, y)) * 0.4
                + (h(x, y.saturating_sub(1)) - h(x, y + 1)) * 0.3;
            let shade = (0.85 + slope / 40.0).clamp(0.45, 1.15);
            let [r, g, b] = t.palette[t.colour[i] as usize];
            [r, g, b]
                .map(|c| (c as f32 * shade).min(255.0) as u8)
                .into_iter()
                .chain([255])
        })
        .collect();
    let tex = Texture2D::from_rgba8(SIZE as u16, SIZE as u16, &bytes);
    tex.set_filter(FilterMode::Nearest);
    tex
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
    let map = args
        .iter()
        .find(|a| !a.starts_with("--") && Some(*a) != shot.as_ref())
        .cloned()
        .or_else(|| maps.first().cloned())
        .unwrap_or_else(|| fail("no maps found in assets/maps"));
    let read = |stem: &str| {
        assets::read_map_image(&map, stem)
            .unwrap_or_else(|e| fail(&format!("{e} (maps: {})", maps.join(", "))))
            .1
    };
    let terrain = Terrain::load(&read("MAP"), &read("COL")).unwrap_or_else(|e| fail(&e));
    let dir = assets::map_dir(&map).unwrap_or_else(|| fail(&format!("can't find map {map:?}")));
    let path = dir.join("CHECK.PCX");
    let mut layer = Layer(match std::fs::read(&path) {
        Ok(b) => pcx::decode(&b)
            .map(|i| i.crop(SIZE, SIZE).pixels)
            .unwrap_or_else(|e| fail(&format!("{}: {e}", path.display()))),
        Err(_) => vec![0; SIZE * SIZE],
    });
    let ground = map_texture(&terrain);
    let overlay = Texture2D::from_rgba8(SIZE as u16, SIZE as u16, &[0; SIZE * SIZE * 4]);
    overlay.set_filter(FilterMode::Nearest);

    let (mut tool, mut dir_spawn, mut strength, mut radius) = (Tool::Gate, 0u8, 8u8, 2i32);
    let (mut zoom, mut pan) = (1.0f32, vec2(0.0, 0.0));
    let mut undo: Vec<Layer> = vec![];
    let mut saved = layer.clone();
    let mut drag: Option<(i32, i32)> = None; // gate line start
    let mut pan_from: Option<(Vec2, Vec2)> = None;
    let mut dirty = true; // the overlay and the check need redoing
    let mut check = String::new();
    let mut note = (String::new(), 0.0);

    loop {
        let (w, h) = (screen_width(), screen_height());
        let top = 54.0;
        // the map fills the window below the help bar, zoomed and panned
        let scale = (w.min(h - top) / SIZE as f32) * zoom;
        let origin = vec2(
            (w - SIZE as f32 * scale) / 2.0,
            top + (h - top - SIZE as f32 * scale) / 2.0,
        ) + pan;
        let mouse = Vec2::from(mouse_position());
        let cell = ((mouse - origin) / scale).floor();
        let (cx, cy) = (cell.x as i32, cell.y as i32);
        let on_map = (0..SIZE as i32).contains(&cx) && (0..SIZE as i32).contains(&cy);

        // keys
        let cmd = is_key_down(KeyCode::LeftSuper)
            || is_key_down(KeyCode::RightSuper)
            || is_key_down(KeyCode::LeftControl)
            || is_key_down(KeyCode::RightControl);
        for (i, k) in [
            KeyCode::Key1,
            KeyCode::Key2,
            KeyCode::Key3,
            KeyCode::Key4,
            KeyCode::Key5,
            KeyCode::Key6,
            KeyCode::Key7,
        ]
        .iter()
        .enumerate()
        {
            if is_key_pressed(*k) {
                tool = TOOLS[i].0;
            }
        }
        for (k, d) in [
            (KeyCode::Up, 0),
            (KeyCode::Right, 1),
            (KeyCode::Down, 2),
            (KeyCode::Left, 3),
        ] {
            if is_key_pressed(k) {
                dir_spawn = d;
                if let Some((x, y, _)) = layer.spawn() {
                    undo.push(layer.clone());
                    layer.set_spawn(x.round() as i32, y.round() as i32, d);
                    dirty = true;
                }
            }
        }
        if is_key_pressed(KeyCode::LeftBracket) {
            strength = strength.saturating_sub(1);
        }
        if is_key_pressed(KeyCode::RightBracket) {
            strength = (strength + 1).min(15);
        }
        if is_key_pressed(KeyCode::Minus) {
            radius = (radius - 1).max(0);
        }
        if is_key_pressed(KeyCode::Equal) {
            radius = (radius + 1).min(12);
        }
        if is_key_pressed(KeyCode::Z) && (cmd || !is_key_down(KeyCode::LeftShift)) {
            if let Some(l) = undo.pop() {
                layer = l;
                dirty = true;
            }
        }
        if is_key_pressed(KeyCode::S) {
            match std::fs::write(&path, layer.encode()) {
                Ok(()) => {
                    saved = layer.clone();
                    note = (format!("saved {}", path.display()), get_time() + 3.0);
                }
                Err(e) => {
                    note = (
                        format!("can't save {}: {e}", path.display()),
                        get_time() + 6.0,
                    )
                }
            }
        }
        if is_key_pressed(KeyCode::Key0) {
            (zoom, pan) = (1.0, vec2(0.0, 0.0));
        }

        // zoom at the cursor, pan with the middle button (or Space + left)
        let (_, wheel) = mouse_wheel();
        if wheel != 0.0 {
            let before = (mouse - origin) / scale;
            zoom = (zoom * if wheel > 0.0 { 1.15 } else { 1.0 / 1.15 }).clamp(0.5, 12.0);
            let scale2 = (w.min(h - top) / SIZE as f32) * zoom;
            let origin2 = vec2(
                (w - SIZE as f32 * scale2) / 2.0,
                top + (h - top - SIZE as f32 * scale2) / 2.0,
            );
            pan = mouse - origin2 - before * scale2;
        }
        let panning = is_mouse_button_down(MouseButton::Middle)
            || (is_key_down(KeyCode::Space) && is_mouse_button_down(MouseButton::Left));
        match (&pan_from, panning) {
            (None, true) => pan_from = Some((mouse, pan)),
            (Some((m, p)), true) => pan = *p + mouse - *m,
            (_, false) => pan_from = None,
        }

        // painting
        if !panning && on_map {
            let left_pressed = is_mouse_button_pressed(MouseButton::Left);
            let left_down = is_mouse_button_down(MouseButton::Left);
            if left_pressed {
                undo.push(layer.clone());
                if undo.len() > 100 {
                    undo.remove(0);
                }
            }
            match tool {
                Tool::Spawn if left_pressed => {
                    layer.set_spawn(cx, cy, dir_spawn);
                    dirty = true;
                }
                Tool::Gate => {
                    if left_pressed {
                        drag = Some((cx, cy));
                    }
                    if is_mouse_button_pressed(MouseButton::Right)
                        && layer.get(cx as usize, cy as usize) >> 4 == GATE
                    {
                        undo.push(layer.clone());
                        layer.erase_gate(layer.get(cx as usize, cy as usize) & 15);
                        dirty = true;
                    }
                }
                Tool::Zone(z) if left_down => {
                    let setting = if z == BOOST || z == SLOW { strength } else { 0 };
                    layer.brush(cx, cy, radius, z << 4 | setting);
                    dirty = true;
                }
                Tool::Erase if left_down => {
                    layer.brush(cx, cy, radius, 0);
                    dirty = true;
                }
                _ => {}
            }
        }
        if let Some(a) = drag
            && is_mouse_button_released(MouseButton::Left)
        {
            drag = None;
            match layer.next_gate() {
                Some(n) => {
                    layer.gate_line(a, (cx.clamp(0, 255), cy.clamp(0, 255)), n);
                    dirty = true;
                }
                None => {
                    note = (
                        "16 gates is the most CHECK.PCX holds".into(),
                        get_time() + 4.0,
                    )
                }
            }
        }

        if dirty {
            dirty = false;
            let rgba: Vec<u8> = layer
                .0
                .iter()
                .flat_map(|&v| {
                    let c = colour(v);
                    [c.r, c.g, c.b, c.a].map(|x| (x * 255.0) as u8)
                })
                .collect();
            overlay.update(&Image {
                width: SIZE as u16,
                height: SIZE as u16,
                bytes: rgba,
            });
            check = match Track::load(&layer.encode()) {
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

        // drawing
        clear_background(Color::from_rgba(24, 26, 30, 255));
        let size = vec2(SIZE as f32, SIZE as f32) * scale;
        for tex in [&ground, &overlay] {
            draw_texture_ex(
                tex,
                origin.x,
                origin.y,
                WHITE,
                DrawTextureParams {
                    dest_size: Some(size),
                    ..Default::default()
                },
            );
        }
        let at = |x: f32, y: f32| origin + vec2(x + 0.5, y + 0.5) * scale;
        // gate numbers in lap order (S = start/finish), at each gate's middle
        for (rank, n) in layer.gates().into_iter().enumerate() {
            let cells: Vec<usize> = (0..SIZE * SIZE)
                .filter(|&i| layer.0[i] == GATE << 4 | n)
                .collect();
            let m = cells.len() as f32;
            let (x, y) = cells.iter().fold((0.0, 0.0), |(x, y), &i| {
                (x + (i % SIZE) as f32 / m, y + (i / SIZE) as f32 / m)
            });
            let label = if rank == 0 {
                "S".to_string()
            } else {
                rank.to_string()
            };
            let p = at(x, y);
            draw_circle(p.x, p.y, 11.0, Color::new(0.0, 0.0, 0.0, 0.7));
            let d = measure_text(&label, None, 20, 1.0);
            draw_text(&label, p.x - d.width / 2.0, p.y + 6.0, 20.0, YELLOW);
        }
        if let Some((x, y, d)) = layer.spawn() {
            let p = at(x, y);
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
        if let Some(a) = drag {
            let (p, q) = (at(a.0 as f32, a.1 as f32), at(cx as f32, cy as f32));
            draw_line(p.x, p.y, q.x, q.y, 3.0, YELLOW);
        }
        if on_map && matches!(tool, Tool::Zone(_) | Tool::Erase) {
            let p = at(cx as f32, cy as f32);
            draw_circle_lines(p.x, p.y, (radius as f32 + 0.5) * scale, 1.5, WHITE);
        }

        // help bar and status
        draw_rectangle(0.0, 0.0, w, top, Color::new(0.0, 0.0, 0.0, 0.8));
        let name = TOOLS.iter().find(|t| t.0 == tool).unwrap().1;
        let unsaved = if layer != saved { "  (unsaved: S)" } else { "" };
        let detail = match tool {
            Tool::Spawn => format!(
                "facing {}",
                ["up", "right", "down", "left"][dir_spawn as usize]
            ),
            Tool::Gate => "drag a line; right-click deletes".into(),
            Tool::Zone(z) if z == BOOST || z == SLOW => {
                format!("strength {strength}, brush {radius}")
            }
            _ => format!("brush {radius}"),
        };
        draw_text(
            &format!("{map}: {name}, {detail}   |   {check}{unsaved}"),
            12.0,
            22.0,
            22.0,
            WHITE,
        );
        draw_text(
            "1 spawn 2 gate 3 reset 4 boost 5 slow 6 soft dirt 7 eraser   arrows: spawn facing   \
             [ ] strength   - = brush   wheel zoom, middle-drag pan, 0 fit   Z undo   S save",
            12.0,
            44.0,
            16.0,
            GRAY,
        );
        if let Some(f) = &shot
            && get_time() > 1.0
        {
            get_screen_data().export_png(f);
            break;
        }
        if get_time() < note.1 {
            let d = measure_text(&note.0, None, 24, 1.0);
            draw_text(&note.0, (w - d.width) / 2.0, h - 24.0, 24.0, YELLOW);
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
    use reformers::track::Zone;

    /// What the editor saves, the game loads: spawn, gates in lap order, zones.
    #[test]
    fn saves_what_the_game_reads() {
        let mut l = Layer(vec![0; SIZE * SIZE]);
        l.set_spawn(100, 120, 1);
        l.set_spawn(128, 140, 2); // a second spawn replaces the first
        assert_eq!(l.next_gate(), Some(0));
        l.gate_line((120, 130), (136, 130), 0);
        l.gate_line((200, 60), (200, 76), l.next_gate().unwrap());
        l.gate_line((40, 60), (56, 76), l.next_gate().unwrap());
        l.brush(60, 200, 2, BOOST << 4 | 9);
        l.brush(90, 200, 1, RESET << 4);
        l.brush(150, 220, 3, SOFT << 4);
        let t = Track::load(&l.encode()).unwrap();
        let s = t.spawn.as_ref().unwrap();
        assert_eq!(
            ((s.x / 256) as i32, (s.y / 256) as i32, s.heading),
            (128, 140, 0x8000)
        );
        assert_eq!(t.gates.len(), 3);
        let cell = |g: usize| t.gates[g][0].centre.map(|v| (v / 256.0) as i32);
        assert_eq!(cell(0), [128, 130], "start/finish is gate 0");
        assert_eq!(cell(1), [200, 68]);
        assert_eq!(t.zone_at(60, 200), Some(Zone::Boost(9)));
        assert_eq!(t.zone_at(90, 200), Some(Zone::Reset));
        assert!(t.soft_dirt[220 * SIZE + 150]);
        // deleting a gate leaves the rest in order; the next one fills in after the last
        l.erase_gate(1);
        assert_eq!(l.gates(), [0, 2]);
        assert_eq!(l.next_gate(), Some(3));
        assert_eq!(Track::load(&l.encode()).unwrap().gates.len(), 2);
        // and the eraser clears anything
        l.brush(60, 200, 3, 0);
        assert_eq!(Track::load(&l.encode()).unwrap().zone_at(60, 200), None);
    }

    #[test]
    fn sixteen_gates_at_most() {
        let mut l = Layer(vec![0; SIZE * SIZE]);
        for n in 0..16 {
            l.gate_line((n * 10, 5), (n * 10, 15), l.next_gate().unwrap());
        }
        assert_eq!(l.next_gate(), None);
        l.erase_gate(4);
        assert_eq!(l.next_gate(), Some(4), "the gap");
    }
}
