//! Checkpoint layer (CHECK.PCX) and the time-trial rules.
//!
//! CHECK.PCX is a 256×256 indexed image on the MAP/COL grid (plus the usual legend strip on the
//! right). Same convention as COL.PCX: index >> 4 = element, index & 15 = setting.
//! - row 1 (16–31): spawn; setting 0–3 = facing up / right / down / left on the image
//! - row 2 (32–47): gate; setting = lap order (sorted, gaps allowed), the lowest is start/finish
//!   (paint one number on several branches for a fork: passing any branch counts)
//! - row 3 (48–63): reset zone; driving in puts the car back like R
//! - row 4 (64–79): boost pad; setting = strength
//! - row 5 (80–95): slow zone (mud); setting = strength
//! - row 6 (96–111): soft dirt; wheels dig ruts into it on every pass
//! - everything else is ignored (row 0 = nothing, rows 7–15 reserved)

use crate::{pcx, terrain::SIZE};

/// CHECK.PCX elements (index >> 4), see above; public for the map editor.
pub const SPAWN: u8 = 1;
pub const GATE: u8 = 2;
pub const RESET: u8 = 3;
pub const BOOST: u8 = 4;
pub const SLOW: u8 = 5;
pub const SOFT: u8 = 6;

/// A painted area that acts on the car while it's inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zone {
    Reset,
    /// Strength 0–15.
    Boost(u8),
    /// Strength 0–15.
    Slow(u8),
}
const CELL: f32 = 256.0; // world units per cell

pub struct Spawn {
    pub x: u16,
    pub y: u16,
    /// Clockwise from image-up (the original's spawn direction), 0x10000 per turn.
    pub heading: u16,
}

/// One connected stretch of a gate. A gate painted on several branches of a fork has a piece
/// on each; driving through any of them counts.
pub struct Piece {
    /// World x, y.
    pub centre: [f32; 2],
    /// The two end cells (world x, y of the cell centres): where the posts stand.
    pub ends: [[f32; 2]; 2],
    /// The painted cells.
    pub cells: Vec<(usize, usize)>,
}

pub struct Track {
    pub spawn: Option<Spawn>,
    /// Gates in lap order, each one piece or more; `gates[0]` is start/finish.
    pub gates: Vec<Vec<Piece>>,
    /// Per cell: lap-order index + 1 of the gate painted there, 0 = none.
    cells: Vec<u8>,
    /// Per cell: the raw CHECK.PCX index where it paints a zone (rows 3–5), else 0.
    zones: Vec<u8>,
    /// Per cell: soft dirt (row 6), for `Sim::set_soft_dirt`.
    pub soft_dirt: Vec<bool>,
}

fn cell(p: [f32; 2]) -> (i32, i32) {
    ((p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32)
}

/// The connected pieces of gate `g` (cells holding `g`). Cells up to 2 apart join, so a line
/// painted with a small gap stays one piece.
fn pieces(cells: &[u8], g: u8) -> Vec<Vec<(usize, usize)>> {
    let mut seen = vec![false; cells.len()];
    let mut out = vec![];
    for start in 0..cells.len() {
        if cells[start] != g || seen[start] {
            continue;
        }
        seen[start] = true;
        let (mut todo, mut piece) = (vec![start], vec![]);
        while let Some(i) = todo.pop() {
            let (x, y) = (i % SIZE, i / SIZE);
            piece.push((x, y));
            for ny in y.saturating_sub(2)..(y + 3).min(SIZE) {
                for nx in x.saturating_sub(2)..(x + 3).min(SIZE) {
                    let j = ny * SIZE + nx;
                    if cells[j] == g && !seen[j] {
                        seen[j] = true;
                        todo.push(j);
                    }
                }
            }
        }
        out.push(piece);
    }
    out
}

fn piece(cells: Vec<(usize, usize)>) -> Piece {
    let world = |(x, y): (usize, usize)| [(x as f32 + 0.5) * CELL, (y as f32 + 0.5) * CELL];
    let n = cells.len() as f32;
    let (sx, sy) = cells.iter().fold((0.0, 0.0), |(sx, sy), &(x, y)| {
        (sx + x as f32, sy + y as f32)
    });
    // ends: the extreme cells along the longer side of the line's bounding box
    let span = |f: fn(&(usize, usize)) -> usize| {
        cells.iter().map(f).max().unwrap() - cells.iter().map(f).min().unwrap()
    };
    let wide = span(|p| p.0) >= span(|p| p.1);
    let key = |p: &&(usize, usize)| if wide { p.0 } else { p.1 };
    let (a, b) = (cells.iter().min_by_key(key), cells.iter().max_by_key(key));
    Piece {
        centre: [(sx / n + 0.5) * CELL, (sy / n + 0.5) * CELL],
        ends: [world(*a.unwrap()), world(*b.unwrap())],
        cells,
    }
}

impl Track {
    pub fn load(check_pcx: &[u8]) -> Result<Track, String> {
        let img = pcx::decode_image(check_pcx)?;
        if img.width < SIZE || img.height < SIZE {
            return Err(format!(
                "CHECK.PCX is {}×{}, needs at least {SIZE}×{SIZE}",
                img.width, img.height
            ));
        }
        let img = img.crop(SIZE, SIZE);
        let mut spawn: Option<(u8, [usize; 4], [f64; 3])> = None; // setting, bbox, (Σx, Σy, n)
        let mut present = [false; 16];
        for (i, &p) in img.pixels.iter().enumerate() {
            let (x, y, setting) = (i % SIZE, i / SIZE, p & 15);
            match p >> 4 {
                SPAWN => {
                    if setting > 3 {
                        return Err(format!(
                            "CHECK.PCX: spawn colour {p} at ({x}, {y}) has no direction (only {}–{} are used)",
                            SPAWN << 4,
                            (SPAWN << 4) + 3
                        ));
                    }
                    let s = spawn.get_or_insert((setting, [x, y, x, y], [0.0; 3]));
                    if s.0 != setting {
                        let (sx, sy) = (s.2[0] / s.2[2], s.2[1] / s.2[2]);
                        return Err(format!(
                            "CHECK.PCX: two spawns, near ({sx:.0}, {sy:.0}) and at ({x}, {y}); paint one"
                        ));
                    }
                    s.1 = [s.1[0].min(x), s.1[1].min(y), s.1[2].max(x), s.1[3].max(y)];
                    s.2[0] += x as f64;
                    s.2[1] += y as f64;
                    s.2[2] += 1.0;
                }
                GATE => present[setting as usize] = true,
                _ => {}
            }
        }
        let centre = |s: &[f64; 3]| {
            [
                ((s[0] / s[2]) as f32 + 0.5) * CELL,
                ((s[1] / s[2]) as f32 + 0.5) * CELL,
            ]
        };
        let spawn = match spawn {
            // ponytail: a bbox check stands in for counting blobs; two blobs of one colour are far apart
            Some((_, [x0, y0, x1, y1], _)) if x1 - x0 > 8 || y1 - y0 > 8 => {
                return Err(format!(
                    "CHECK.PCX: spawn pixels spread from ({x0}, {y0}) to ({x1}, {y1}); paint one small blob"
                ));
            }
            Some((setting, _, sum)) => {
                let [x, y] = centre(&sum);
                Some(Spawn {
                    x: x as u16,
                    y: y as u16,
                    heading: setting as u16 * 0x4000,
                })
            }
            None => None,
        };
        // lap order: present settings, sorted
        let order: Vec<usize> = (0..16).filter(|&s| present[s]).collect();
        let mut rank = [0u8; 16];
        for (r, &s) in order.iter().enumerate() {
            rank[s] = r as u8 + 1;
        }
        let cells: Vec<u8> = img
            .pixels
            .iter()
            .map(|&p| {
                if p >> 4 == GATE {
                    rank[(p & 15) as usize]
                } else {
                    0
                }
            })
            .collect();
        let gates = (1..=order.len() as u8)
            .map(|g| pieces(&cells, g).into_iter().map(piece).collect())
            .collect();
        let zones = img
            .pixels
            .iter()
            .map(|&p| {
                if (RESET..=SLOW).contains(&(p >> 4)) {
                    p
                } else {
                    0
                }
            })
            .collect();
        Ok(Track {
            spawn,
            gates,
            cells,
            zones,
            soft_dirt: img.pixels.iter().map(|&p| p >> 4 == SOFT).collect(),
        })
    }

    /// The piece of gate `g` nearest to world point `p`.
    // ponytail: straight-line distance, not wrapped at the map edge; gates sit well inside it
    pub fn piece(&self, g: usize, p: [f32; 2]) -> &Piece {
        let d = |q: &&Piece| (q.centre[0] - p[0]).hypot(q.centre[1] - p[1]);
        self.gates[g]
            .iter()
            .min_by(|a, b| d(a).total_cmp(&d(b)))
            .unwrap()
    }

    /// The zone painted on cell (x, y), if any.
    pub fn zone_at(&self, x: usize, y: usize) -> Option<Zone> {
        let p = self.zones[y * SIZE + x];
        match p >> 4 {
            RESET => Some(Zone::Reset),
            BOOST => Some(Zone::Boost(p & 15)),
            SLOW => Some(Zone::Slow(p & 15)),
            _ => None,
        }
    }

    /// The zone under world point `p` (wrapping at the map edge like the terrain).
    pub fn zone_under(&self, p: [f32; 2]) -> Option<Zone> {
        let (x, y) = cell(p);
        self.zone_at(
            x.rem_euclid(SIZE as i32) as usize,
            y.rem_euclid(SIZE as i32) as usize,
        )
    }

    /// One tick of the zone under the car: boost pads push it, mud drags it. True on a reset zone
    /// (the caller respawns the car like R).
    pub fn apply_zone(&self, car: &mut crate::sim::SimCar) -> bool {
        let [x, y, _] = car.centre();
        match self.zone_under([x, y]) {
            // strength 0–15: up to 1/16 world unit per tick² / a 1/8 speed loss per tick
            Some(Zone::Boost(s)) => car.push((s as i32 + 1) * 0x100),
            Some(Zone::Slow(s)) => car.drag(8 - s as u32 / 3),
            Some(Zone::Reset) => return true,
            None => {}
        }
        false
    }

    /// Lap-order index of the gate painted on cell (x, y), if any.
    pub fn gate_at(&self, x: usize, y: usize) -> Option<usize> {
        self.cells[y * SIZE + x].checked_sub(1).map(usize::from)
    }

    /// Bitmask (by lap-order index) of gates on the cells between world points `a` and `b`.
    /// Checks the whole cell box, so a diagonal step can't slip between the pixels of a 1 px line.
    fn touched(&self, a: [f32; 2], b: [f32; 2]) -> u16 {
        let ((ax, ay), (bx, by)) = (cell(a), cell(b));
        // ponytail: a car moves < 0.1 cell per tick; anything longer is a respawn or the map wrapping
        if (ax - bx).abs() > 2 || (ay - by).abs() > 2 {
            return 0;
        }
        let mut mask = 0;
        for y in ay.min(by)..=ay.max(by) {
            for x in ax.min(bx)..=ax.max(bx) {
                let (x, y) = (x.rem_euclid(SIZE as i32), y.rem_euclid(SIZE as i32));
                if let Some(g) = self.gate_at(x as usize, y as usize) {
                    mask |= 1 << g;
                }
            }
        }
        mask
    }
}

pub struct Lap {
    /// Ticks (sim steps, `TICK_HZ` per second).
    pub time: u64,
    /// Ticks from lap start at each gate after start/finish.
    pub splits: Vec<u64>,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Start,
    Gate(usize),
    Lap { time: u64, best: bool },
}

/// Time-trial state: `next` is the only gate that counts; driving through any other does nothing.
pub struct Race {
    pub next: usize,
    pub started: bool,
    /// Completed laps.
    pub laps: u32,
    pub last_gate: Option<usize>,
    /// Where the car passed `last_gate`: picks the piece of a forked gate.
    passed_at: [f32; 2],
    pub last_lap: Option<u64>,
    pub best: Option<Lap>,
    lap_start: u64,
    splits: Vec<u64>,
    pos: [f32; 2],
    touching: u16,
}

impl Race {
    pub fn new(best: Option<Lap>) -> Race {
        Race {
            next: 0,
            started: false,
            laps: 0,
            last_gate: None,
            passed_at: [0.0; 2],
            last_lap: None,
            best,
            lap_start: 0,
            splits: vec![],
            pos: [f32::NAN; 2],
            touching: 0,
        }
    }

    /// Puts the car at `pos` without counting gates (spawn, respawn).
    pub fn place(&mut self, track: &Track, pos: [f32; 2]) {
        self.pos = pos;
        self.touching = track.touched(pos, pos);
    }

    /// One sim tick with the car centre at `pos`. A gate counts when the car enters it.
    pub fn update(&mut self, track: &Track, pos: [f32; 2], tick: u64) -> Option<Event> {
        let n = track.gates.len();
        if self.pos[0].is_nan() {
            self.place(track, pos);
        }
        let entered = track.touched(self.pos, pos) & !self.touching;
        self.place(track, pos);
        if n == 0 || entered & (1 << self.next) == 0 {
            return None;
        }
        let gate = self.next;
        self.last_gate = Some(gate);
        self.passed_at = pos;
        self.next = (gate + 1) % n;
        if !self.started {
            self.started = true;
            self.lap_start = tick;
            return Some(Event::Start);
        }
        if gate != 0 {
            self.splits.push(tick - self.lap_start);
            return Some(Event::Gate(gate));
        }
        let time = tick - self.lap_start;
        let best = self.best.as_ref().is_none_or(|b| time < b.time);
        if best {
            self.best = Some(Lap {
                time,
                splits: self.splits.clone(),
            });
        }
        self.splits.clear();
        self.last_lap = Some(time);
        self.laps += 1;
        self.lap_start = tick;
        Some(Event::Lap { time, best })
    }

    /// How far round the track the car is, for placings: completed laps × gates, plus gates
    /// passed this lap, plus the share of the way from the last gate to the next one.
    pub fn progress(&self, track: &Track, pos: [f32; 2]) -> f32 {
        let n = track.gates.len().max(1);
        if !self.started {
            return 0.0;
        }
        let passed = (self.next + n - 1) % n;
        let from = track
            .piece(self.last_gate.unwrap_or(0), self.passed_at)
            .centre;
        let to = track.piece(self.next, pos).centre;
        let dist = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
        let part = 1.0 - (dist(pos, to) / dist(from, to).max(1.0)).min(1.0);
        (self.laps as usize * n + passed) as f32 + part
    }

    /// Ticks into the current lap (0 before the start).
    pub fn lap_time(&self, tick: u64) -> u64 {
        if self.started {
            tick - self.lap_start
        } else {
            0
        }
    }

    /// Last split minus the best lap's split at the same gate, in ticks.
    pub fn split_delta(&self) -> Option<i64> {
        let i = self.splits.len().checked_sub(1)?;
        let best = self.best.as_ref()?.splits.get(i)?;
        Some(self.splits[i] as i64 - *best as i64)
    }

    /// Where R puts the car: on the piece of the last gate it passed, facing the nearest piece of
    /// the next one; else the spawn.
    pub fn respawn(&self, track: &Track) -> Option<([f32; 2], u16)> {
        match self.last_gate {
            Some(g) => {
                let [x, y] = track.piece(g, self.passed_at).centre;
                let [nx, ny] = track.piece(self.next, [x, y]).centre;
                let heading = if g == self.next {
                    0
                } else {
                    crate::sim::heading_of((nx - x) as i32, (ny - y) as i32)
                };
                Some(([x, y], heading))
            }
            None => track
                .spawn
                .as_ref()
                .map(|s| ([s.x as f32, s.y as f32], s.heading)),
        }
    }
}

/// `ticks` as `m:ss.cc`.
pub fn format_time(ticks: u64) -> String {
    let cs = ticks * 100 / crate::sim::TICK_HZ as u64;
    format!("{}:{:02}.{:02}", cs / 6000, cs / 100 % 60, cs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Keys, Sim, SimCar, TICK_HZ};
    use crate::terrain::Terrain;

    /// Minimal 8-bit PCX writer: one literal run per byte (≥ 0xC0 escaped).
    fn pcx(w: usize, h: usize, pixels: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 128];
        d[0] = 0x0A;
        d[1] = 5;
        d[2] = 1;
        d[3] = 8;
        d[8..10].copy_from_slice(&(w as u16 - 1).to_le_bytes());
        d[10..12].copy_from_slice(&(h as u16 - 1).to_le_bytes());
        d[65] = 1;
        d[66..68].copy_from_slice(&(w as u16).to_le_bytes());
        for &p in pixels {
            if p >= 0xC0 {
                d.push(0xC1);
            }
            d.push(p);
        }
        d.push(0x0C);
        d.extend([0u8; 768]);
        d
    }

    fn track(paint: &[(usize, usize, u8)]) -> Result<Track, String> {
        let mut px = vec![0u8; SIZE * SIZE];
        for &(x, y, c) in paint {
            px[y * SIZE + x] = c;
        }
        Track::load(&pcx(SIZE, SIZE, &px))
    }

    /// World point at the centre of cell (x, y).
    fn at(x: f32, y: f32) -> [f32; 2] {
        [(x + 0.5) * CELL, (y + 0.5) * CELL]
    }

    #[test]
    fn decodes_spawn_and_gate_order() {
        let mut paint = vec![(10, 20, 17), (11, 20, 17), (10, 21, 17), (11, 21, 17)]; // spawn, facing right
        paint.extend((0..5).map(|x| (100 + x, 50, 32 + 9))); // order 9
        paint.extend((0..5).map(|x| (100 + x, 60, 32 + 2))); // order 2: first, start/finish
        paint.extend((0..5).map(|x| (100 + x, 70, 32 + 15)));
        let t = track(&paint).unwrap();
        let s = t.spawn.as_ref().unwrap();
        assert_eq!((s.x, s.y), (11 * 256, 21 * 256)); // centroid (10.5, 20.5) + half a cell
        assert_eq!(s.heading, 0x4000);
        assert_eq!(t.gates.len(), 3);
        assert_eq!(t.gates[0][0].centre, at(102.0, 60.0));
        assert_eq!(t.gates[1][0].centre, at(102.0, 50.0));
        assert_eq!(t.gate_at(100, 70), Some(2));
        assert_eq!(t.gates[0][0].ends, [at(100.0, 60.0), at(104.0, 60.0)]);
        let diag = track(&(0..5).map(|i| (50 + i, 90 - 2 * i, 32)).collect::<Vec<_>>()).unwrap();
        assert_eq!(diag.gates[0][0].ends, [at(54.0, 82.0), at(50.0, 90.0)]); // steeper than 45°: by y
        assert_eq!(t.gate_at(99, 70), None);
        let z = track(&[(5, 5, 48), (6, 5, 64 + 9), (7, 5, 80 + 3), (8, 5, 96)]).unwrap();
        assert_eq!(z.zone_at(5, 5), Some(Zone::Reset));
        assert_eq!(z.zone_at(6, 5), Some(Zone::Boost(9)));
        assert_eq!(z.zone_under(at(7.0, 5.0)), Some(Zone::Slow(3)));
        assert_eq!(z.zone_at(8, 5), None, "row 6 is soft dirt, not a zone");
        assert!(z.soft_dirt[5 * SIZE + 8] && !z.soft_dirt[5 * SIZE + 7]);
    }

    #[test]
    fn rejects_bad_spawns() {
        let two = track(&[(10, 10, 16), (200, 200, 18)]).err().unwrap();
        assert!(two.contains("two spawns"), "{two}");
        let spread = track(&[(10, 10, 16), (200, 200, 16)]).err().unwrap();
        assert!(spread.contains("spread"), "{spread}");
        assert!(
            track(&[(10, 10, 20)]).is_err(),
            "setting 4 has no direction"
        );
        assert!(track(&[]).unwrap().spawn.is_none());
    }

    #[test]
    fn counts_gates_in_order_only() {
        // three horizontal gates, driven upwards (−y) along column 100
        let paint: Vec<_> = [(150, 32), (120, 33), (90, 34)]
            .iter()
            .flat_map(|&(y, c)| (95..106).map(move |x| (x, y, c)))
            .collect();
        let t = track(&paint).unwrap();
        let mut r = Race::new(None);
        let mut tick = 0u64;
        let drive = |r: &mut Race, tick: &mut u64, from: f32, to: f32| {
            let mut ev = vec![];
            let step: f32 = if to < from { -0.05 } else { 0.05 };
            let mut y = from;
            while (to - y) * step.signum() > 0.0 {
                y += step;
                *tick += 1;
                ev.extend(r.update(&t, at(100.0, y), *tick));
            }
            ev
        };
        assert_eq!(drive(&mut r, &mut tick, 160.0, 125.0), [Event::Start]);
        // back through start, then on: start doesn't count again, gate 1 does
        assert_eq!(drive(&mut r, &mut tick, 125.0, 155.0), []);
        assert_eq!(drive(&mut r, &mut tick, 155.0, 115.0), [Event::Gate(1)]);
        // skip gate 2 by respawning past it: the lap doesn't complete
        r.place(&t, at(100.0, 85.0));
        assert_eq!(drive(&mut r, &mut tick, 85.0, 80.0), []);
        // wrap around (the map is a loop): through 2, then start/finish
        r.place(&t, at(100.0, 95.0));
        let ev = drive(&mut r, &mut tick, 95.0, 85.0);
        assert_eq!(ev, [Event::Gate(2)]);
        r.place(&t, at(100.0, 155.0));
        let ev = drive(&mut r, &mut tick, 155.0, 145.0);
        assert!(matches!(ev[..], [Event::Lap { best: true, .. }]), "{ev:?}");
        assert_eq!(
            (r.laps, r.next, r.best.as_ref().unwrap().splits.len()),
            (1, 1, 2)
        );
        // second lap slower: not best; split delta compares gate 1 against lap 1
        r.place(&t, at(100.0, 125.0));
        tick += 10_000;
        assert_eq!(drive(&mut r, &mut tick, 125.0, 115.0), [Event::Gate(1)]);
        assert!(r.split_delta().unwrap() > 0);
    }

    #[test]
    fn progress_orders_cars() {
        let t = track(&[(100, 100, 32), (100, 60, 33), (100, 20, 34)]).unwrap();
        let (mut a, mut b) = (Race::new(None), Race::new(None));
        assert_eq!(a.progress(&t, at(100.0, 110.0)), 0.0, "not started");
        for r in [&mut a, &mut b] {
            r.place(&t, at(100.0, 101.0));
            r.update(&t, at(100.0, 100.0), 1); // start
        }
        a.place(&t, at(100.0, 61.0));
        a.update(&t, at(100.0, 60.0), 2); // a passes gate 1
        let (pa, pb) = (
            a.progress(&t, at(100.0, 50.0)),
            b.progress(&t, at(100.0, 80.0)),
        );
        assert!(pa > pb, "a is ahead: {pa} vs {pb}");
        assert!(
            b.progress(&t, at(100.0, 70.0)) > pb,
            "closer to the next gate is further on"
        );
    }

    #[test]
    fn respawn_faces_next_gate() {
        let t = track(&[(100, 100, 32), (150, 100, 33), (20, 20, 18)]).unwrap();
        let mut r = Race::new(None);
        let (p, h) = r.respawn(&t).unwrap();
        assert_eq!((p, h), (at(20.0, 20.0), 0x8000)); // spawn, facing down
        r.place(&t, at(100.0, 101.0));
        r.update(&t, at(100.0, 100.0), 1);
        let (p, h) = r.respawn(&t).unwrap();
        assert_eq!(p, at(100.0, 100.0));
        assert_eq!(h, 0x4000, "next gate is to the right");
    }

    #[test]
    fn thin_gate_counts_at_top_speed() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        let mut s = Sim::new(Terrain {
            height: vec![40; SIZE * SIZE],
            colour: vec![0x60; SIZE * SIZE],
            palette: [[0; 3]; 256],
        });
        s.cars.push(SimCar::spawn(
            &car,
            &s.terrain,
            (128 * 256) as u16,
            (200 * 256) as u16,
        ));
        // 1 px diagonal line across the path: 8-connected, so a diagonal step could slip through
        let t = track(&(0..60).map(|i| (100 + i, 140 + i, 32)).collect::<Vec<_>>()).unwrap();
        let mut r = Race::new(None);
        let gas = Keys {
            throttle: true,
            ..Keys::default()
        };
        let mut started = false;
        for tick in 0..TICK_HZ as u64 * 30 {
            s.tick(&[gas]);
            let [x, y, _] = s.cars[0].centre();
            started |= r.update(&t, [x, y], tick) == Some(Event::Start);
        }
        let [_, y, _] = s.cars[0].centre();
        assert!(
            y < 160.0 * CELL,
            "car should have driven past the line, y = {}",
            y / CELL
        );
        assert!(started, "gate crossing missed");
    }

    /// A fork: gate 1 painted on two branches. Either branch counts; the piece the car took is
    /// where R puts it and what progress measures from; one stray-gap line stays one piece.
    #[test]
    fn forked_gate_has_pieces() {
        let mut paint: Vec<_> = (95..106).map(|x| (x, 150, 32)).collect(); // start
        paint.extend((55..66).map(|x| (x, 100, 33))); // left branch
        paint.extend((135..146).map(|x| (x, 100, 33))); // right branch
        paint.extend((95..106).filter(|&x| x != 100).map(|x| (x, 50, 34))); // a 1-cell gap
        let t = track(&paint).unwrap();
        assert_eq!(t.gates.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2, 1]);
        assert_eq!(t.piece(1, at(150.0, 120.0)).centre, at(140.0, 100.0));
        for x in [60.0, 140.0] {
            let mut r = Race::new(None);
            r.place(&t, at(100.0, 151.0));
            r.update(&t, at(100.0, 150.0), 1); // start
            r.place(&t, at(x, 101.0));
            assert_eq!(r.update(&t, at(x, 100.0), 2), Some(Event::Gate(1)));
            let (p, _) = r.respawn(&t).unwrap();
            assert_eq!(p, at(x, 100.0), "R puts the car on the branch it took");
            let near = r.progress(&t, at(x, 90.0));
            assert!(near > 1.0 && near < 2.0, "{near}");
        }
    }

    #[test]
    fn spawn_faces_heading() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        for (k, want) in [[0.0, -1.0], [1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]]
            .iter()
            .enumerate()
        {
            let mut s = Sim::new(Terrain {
                height: vec![40; SIZE * SIZE],
                colour: vec![0x60; SIZE * SIZE],
                palette: [[0; 3]; 256],
            });
            let heading = k as u16 * 0x4000;
            s.cars.push(SimCar::spawn_facing(
                &car, &s.terrain, 0x8000, 0x8000, heading,
            ));
            let start = s.cars[0].centre();
            for _ in 0..TICK_HZ * 3 {
                s.tick(&[Keys {
                    throttle: true,
                    ..Keys::default()
                }]);
            }
            let end = s.cars[0].centre();
            let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
            let len = (dx * dx + dy * dy).sqrt();
            assert!(
                (dx / len - want[0]).abs() < 0.1 && (dy / len - want[1]).abs() < 0.1,
                "heading {k}: drove ({dx}, {dy})"
            );
        }
    }

    /// Steers at the next gate, respawning (like R) when stuck or upside down. Proves the
    /// shipped track can be lapped and that a full lap registers.
    #[test]
    fn shipped_track_laps() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let t = Track::load(&read("maps/terep2/CHECK.PCX")).unwrap();
        assert!(t.gates.len() >= 3);
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let mut s = Sim::new(
            Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap(),
        );
        s.set_soft_dirt(t.soft_dirt.clone());
        assert!(
            t.soft_dirt.iter().any(|&d| d),
            "the shipped track has soft dirt"
        );
        let sp = t.spawn.as_ref().unwrap();
        s.cars.push(SimCar::spawn_facing(
            &car, &s.terrain, sp.x, sp.y, sp.heading,
        ));
        let mut r = Race::new(None);
        let mut last = [0.0f32; 2];
        let (mut respawns, mut resets) = (0, 0);
        let mut mark = [0.0f32; 2];
        for tick in 0..TICK_HZ as u64 * 240 {
            let a = s.cars[0].atom_positions();
            let [x, y, _] = s.cars[0].centre();
            let fwd = [
                a[6][0] + a[7][0] - a[4][0] - a[5][0],
                a[6][1] + a[7][1] - a[4][1] - a[5][1],
            ];
            // pure pursuit along the line from the last gate (or spawn) to the next one
            let g = t.piece(r.next, [x, y]).centre;
            let p = r
                .last_gate
                .map_or([sp.x as f32, sp.y as f32], |i| t.piece(i, [x, y]).centre);
            let seg = [g[0] - p[0], g[1] - p[1]];
            let len = (seg[0] * seg[0] + seg[1] * seg[1]).sqrt().max(1.0);
            let u = [seg[0] / len, seg[1] / len];
            let along = ((x - p[0]) * u[0] + (y - p[1]) * u[1] + 3.0 * CELL).clamp(0.0, len);
            let to = [p[0] + u[0] * along - x, p[1] + u[1] * along - y];
            let cross = fwd[0] * to[1] - fwd[1] * to[0];
            let dot = fwd[0] * to[0] + fwd[1] * to[1];
            let speed = ((x - last[0]).powi(2) + (y - last[1]).powi(2)).sqrt();
            last = [x, y];
            // steering pulses with a duty proportional to the angle error, like tapping the arrows
            let err = cross.atan2(dot);
            let steer = ((tick % 12) as f32) < (err.abs() / 0.35).min(1.0) * 12.0;
            let keys = Keys {
                throttle: speed < 7.0,
                right: steer && err > 0.0,
                left: steer && err < 0.0,
                ..Keys::default()
            };
            s.tick(&[keys]);
            if t.apply_zone(&mut s.cars[0]) {
                resets += 1;
                let ([px, py], h) = r.respawn(&t).unwrap();
                s.cars[0] = SimCar::spawn_facing(&car, &s.terrain, px as u16, py as u16, h);
                r.place(&t, [px, py]);
                continue;
            }
            if let Some(Event::Lap { .. }) = r.update(&t, [x, y], tick) {
                return;
            }
            // stuck (or on its roof): no progress in 4 s → R
            if tick % (TICK_HZ as u64 * 4) == 0 {
                if ((x - mark[0]).powi(2) + (y - mark[1]).powi(2)).sqrt() < CELL {
                    respawns += 1;
                    let ([px, py], h) = r.respawn(&t).unwrap();
                    s.cars[0] = SimCar::spawn_facing(&car, &s.terrain, px as u16, py as u16, h);
                    r.place(&t, [px, py]);
                }
                mark = [x, y];
            }
        }
        panic!(
            "no lap in 4 minutes: next gate {} of {}, {respawns} respawns, {resets} reset zones, car at {:?}",
            r.next,
            t.gates.len(),
            s.cars[0].centre().map(|v| v / CELL)
        );
    }
}
