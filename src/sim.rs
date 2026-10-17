//! Headless physics: a literal 16.16 fixed-point port of TEREP2's 120 Hz timer tick
//! (`1000:5680`). Addresses in comments point at the original asm; docs/re-notes.md explains them.
//! Car–car collision (`0x500b`) is not ported yet.

use crate::car::{AtomKind, Car, Drive, SpringKind};
use crate::terrain::{SIZE, Terrain};

pub const TICK_HZ: u32 = 120;
const GRAVITY: i32 = 0x1800; // [0x6a], set at 0x00e2
const DIG_COLOUR: u8 = 0x0f; // only particles from this ground colour dig (0x0be6)
const MAX_PARTICLES: usize = 0x15e0 / 0x1c;

// Per material (COL >> 4): DS 0xea29 / 0xea49 / 0xea59.
const GRIP: [i32; 16] = [
    16, 25, 20, 40, 30, 25, 16, 20, 40, 25, 25, 25, 16, 25, 16, 16,
];
const DAMP: [u8; 16] = [12, 9, 10, 10, 10, 10, 8, 10, 10, 10, 10, 10, 8, 6, 10, 10];
const MAX_LOAD: [i32; 16] = [
    0x30000, 0x18000, 0x20000, 0xc000, 0x20000, 0x20000, 0x20000, 0x20000, 0x20000, 0x20000,
    0x20000, 0x20000, 0x10000, 0x10000, 0x10000, 0x10000,
];

/// `imul r32` keeping EDX: (a·b) >> 32.
fn hi(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}
/// `imul` + `shl edx, 1`: a Q31 multiply.
fn q31(a: i32, b: i32) -> i32 {
    hi(a, b).wrapping_shl(1)
}

/// Q15 sine of a 0x10000-per-turn angle, like `0x2aad` (table at DS 0xd10c).
fn sin15(angle: u16) -> i16 {
    let quarter = |i: u16| {
        ((i as f64 * std::f64::consts::FRAC_PI_2 / 1024.0).sin() * 32768.0)
            .round()
            .min(32767.0) as i16
    };
    let mut a = angle & 0x7fff;
    if a & 0x4000 != 0 {
        a = 0x8000 - a;
    }
    let v = quarter(a >> 4);
    if angle & 0x8000 != 0 { -v } else { v }
}
fn cos15(angle: u16) -> i16 {
    sin15(angle.wrapping_add(0x4000))
}
/// `0x2b08`: angle of (x = bx, y = ax) in 0x10000-per-turn units.
fn atan2(ax: i16, bx: i16) -> u16 {
    let t = (ax as f64).atan2(bx as f64) / std::f64::consts::TAU * 65536.0;
    t.round() as i64 as u16
}

#[derive(Clone, Copy)]
struct Atom {
    pos: [i32; 3],
    vel: [i32; 3],
    radius: u16,
    kind: u16,
}

#[derive(Clone, Copy)]
struct Spring {
    a: usize,
    b: usize,
    rest: i16,
    aux: u16,
    kind: u8,
    shift: u8,
    min: u16,
    max: u16,
}

#[derive(Clone, Copy, Default)]
pub struct Keys {
    pub throttle: bool,
    pub reverse: bool,
    pub left: bool,
    pub right: bool,
    pub brake: bool,
}

pub struct SimCar {
    atoms: Vec<Atom>,
    springs: Vec<Spring>,
    mode: u16,
    steer: i16,    // +0x0a
    throttle: i16, // +0x0c
    brake: bool,   // +0x0e
    steer_now: i16,
    throttle_now: i16,
    spin: [i32; 4], // +0x42..
}

#[derive(Clone, Copy)]
struct Particle {
    pos: [i32; 3],
    vel: [i32; 3],
    life: i16,
    colour: u16,
}

/// Per-contact scratch that the asm keeps in DS globals.
struct Contact {
    n: [i32; 4], // A, B, C, D = [0xe9a0], [0xe9a4], [0xe9a8], [0xe9ac]
    vn: i32,     // [0xe9c8]
    colour: u8,  // [0xea28]
}

pub struct Sim {
    pub terrain: Terrain,
    pub cars: Vec<SimCar>,
    particles: Vec<Particle>,
    /// sim.cfg: yield above, break below (rest − length, ×64).
    pub cfg: (i16, i16),
    /// Treat every ground colour as diggable. The shipped COL.PCX has no colour 0x0f,
    /// so the original never digs; this switch shows the mechanism.
    pub dig_everywhere: bool,
    /// For `dig_everywhere`: cells still "colour 0x0f". A landing darkens 0x0f → 0x0e in the
    /// original, which stops later particles from that cell digging; this mirrors that.
    soft: Vec<bool>,
}

impl SimCar {
    /// Places a parsed car at world (x, y) like `0x2454`: z = ground + 100, facing the map's −y.
    pub fn spawn(car: &Car, terrain: &Terrain, x: u16, y: u16) -> SimCar {
        Self::spawn_facing(car, terrain, x, y, 0.0)
    }

    /// Like `spawn`, turned `heading` radians clockwise on the map image (0 = −y, the original's facing).
    pub fn spawn_facing(car: &Car, terrain: &Terrain, x: u16, y: u16, heading: f32) -> SimCar {
        let ground = height(terrain, x, y).0 as i32 + 0x64;
        let fx = |v: f32| (v * 65536.0) as i32;
        let (sin, cos) = heading.sin_cos();
        let atoms = car
            .atoms
            .iter()
            .map(|a| Atom {
                pos: [
                    fx(a.pos[0] * cos - a.pos[1] * sin).wrapping_add(((x as u32) << 16) as i32),
                    fx(a.pos[0] * sin + a.pos[1] * cos).wrapping_add(((y as u32) << 16) as i32),
                    fx(a.pos[2]) + (ground << 16),
                ],
                vel: [0; 3],
                radius: a.radius as u16,
                kind: match a.kind {
                    AtomKind::Body => 0,
                    AtomKind::RearWheel => 1,
                    AtomKind::FrontWheel => 2,
                    AtomKind::Driver => 0xffff,
                },
            })
            .collect();
        let springs = car
            .springs
            .iter()
            .map(|s| Spring {
                a: s.a,
                b: s.b,
                rest: s.rest.map_or(-1, |r| (r * 64.0) as i16),
                aux: (s.orig * 64.0) as u16,
                kind: match s.kind {
                    SpringKind::Slack => 0,
                    SpringKind::Rigid => 1,
                    SpringKind::Damped(k) => k,
                },
                shift: s.shift,
                min: (s.min * 64.0) as u16,
                max: (s.max * 64.0) as u16,
            })
            .collect();
        let mode = match car.drive {
            Drive::Rear => 0,
            Drive::Front => 1,
            Drive::All => 2,
        };
        SimCar {
            atoms,
            springs,
            mode,
            steer: 0,
            throttle: 0,
            brake: false,
            steer_now: 0,
            throttle_now: 0,
            spin: [0; 4],
        }
    }

    /// World position of the car: midpoint of atoms 4 and 6, like `0x0e28`. x, y are unsigned 16.16.
    pub fn centre(&self) -> [f32; 3] {
        let (a, b) = (self.atoms[4].pos, self.atoms[6].pos);
        let mid = |k: usize| a[k].wrapping_add(b[k].wrapping_sub(a[k]) / 2);
        [
            mid(0) as u32 as f32 / 65536.0,
            mid(1) as u32 as f32 / 65536.0,
            mid(2) as f32 / 65536.0,
        ]
    }

    /// Atom positions in world units (x, y unsigned 16.16; z signed).
    pub fn atom_positions(&self) -> Vec<[f32; 3]> {
        let f = |a: &Atom| {
            [
                a.pos[0] as u32 as f32 / 65536.0,
                a.pos[1] as u32 as f32 / 65536.0,
                a.pos[2] as f32 / 65536.0,
            ]
        };
        self.atoms.iter().map(f).collect()
    }

    /// (a, b, broken) for each spring.
    pub fn spring_links(&self) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
        self.springs.iter().map(|s| (s.a, s.b, s.kind & 0x80 != 0))
    }

    /// Current steering angle in radians (positive = right).
    pub fn steer_angle(&self) -> f32 {
        self.steer_now as f32 / 65536.0 * std::f32::consts::TAU
    }

    /// Wheel spin of atoms 0–3 (16.16 per tick); the HUD uses it as a speedometer.
    pub fn spin(&self) -> [i32; 4] {
        self.spin
    }

    /// Boost pad: speed every atom up along the car's heading by `accel` (16.16 world units per
    /// tick², so a pad acts for as long as the car is on it).
    pub fn push(&mut self, accel: i32) {
        let d = |i: usize, j: usize, k: usize| {
            self.atoms[i].pos[k].wrapping_sub(self.atoms[j].pos[k]) as f64
        };
        let fwd = [d(6, 4, 0) + d(7, 5, 0), d(6, 4, 1) + d(7, 5, 1)];
        let len = fwd[0].hypot(fwd[1]).max(1.0);
        let dv = fwd.map(|f| (f / len * accel as f64) as i32);
        for a in &mut self.atoms {
            a.vel[0] = a.vel[0].wrapping_add(dv[0]);
            a.vel[1] = a.vel[1].wrapping_add(dv[1]);
        }
    }

    /// Slow zone: lose `v >> shift` of the horizontal speed per tick, like the ground friction.
    pub fn drag(&mut self, shift: u32) {
        for a in &mut self.atoms {
            a.vel[0] -= a.vel[0] >> shift;
            a.vel[1] -= a.vel[1] >> shift;
        }
    }

    /// Roof down: the car's up axis (front × right, from the wheel atoms 4–7) points into the ground.
    /// Unlike comparing two atoms' heights, pitching or a steep slope doesn't trip it.
    pub fn upside_down(&self) -> bool {
        // x, y are unsigned 16.16 held in i32: take wrapping differences, never absolute values
        let d = |i: usize, j: usize, k: usize| {
            self.atoms[i].pos[k].wrapping_sub(self.atoms[j].pos[k]) as f64
        };
        let fwd = [d(6, 4, 0) + d(7, 5, 0), d(6, 4, 1) + d(7, 5, 1)];
        let side = [d(7, 6, 0), d(7, 6, 1)];
        fwd[0] * side[1] - fwd[1] * side[0] < 0.0
    }

    pub fn broken_springs(&self) -> usize {
        self.springs.iter().filter(|s| s.kind & 0x80 != 0).count()
    }

    /// Keyboard → control words (`0x0d2a`).
    fn input(&mut self, k: Keys) {
        let mut bx = self.throttle;
        let mut cx: i16 = 0x32;
        if k.throttle && bx <= 0x2000 {
            if bx < 0 {
                cx <<= 2;
            }
            bx = bx.wrapping_add(cx);
        }
        let mut cx: i16 = 0x32;
        if k.reverse && bx >= -0x2000 {
            if bx >= 0 {
                cx <<= 2;
            }
            bx = bx.wrapping_sub(cx);
        }
        if !k.throttle && !k.reverse && bx != 0 {
            bx = bx.wrapping_sub(if bx < 0 { -0x12c } else { 0x12c });
        }
        self.throttle = bx;

        // Steering rate falls with front-wheel spin. The asm takes |low word| but then
        // the (signed) high word; ported as-is.
        let sum = self.spin[0].wrapping_add(self.spin[1]) as u32;
        let rate = 0x40u16.wrapping_sub((sum >> 16) as u16) as i16;
        let mut bx = self.steer;
        if k.left && bx >= -0x2000 {
            bx = bx.wrapping_sub(if bx >= 0 { rate.wrapping_shl(2) } else { rate });
        }
        if k.right && bx <= 0x2000 {
            bx = bx.wrapping_add(if bx < 0 { rate.wrapping_shl(2) } else { rate });
        }
        if !k.left && !k.right && bx != 0 {
            bx = bx.wrapping_sub(if bx < 0 { -0x50 } else { 0x50 });
        }
        self.steer = bx;
        self.brake = k.brake;
    }

    /// Engine, drag and brakes (`0x1004`).
    fn drive(&mut self) {
        self.steer_now = self.steer;
        self.throttle_now = self.throttle;
        for s in &mut self.spin {
            *s -= *s >> 7;
            if self.brake {
                *s -= *s >> 2;
            }
        }
        let t = ((self.throttle_now as i32 * 0x4000) >> 16) as u32;
        match self.mode {
            0 => [2, 3]
                .iter()
                .for_each(|&w| self.spin[w] = self.spin[w].wrapping_add(t.rotate_left(4) as i32)),
            1 => [0, 1]
                .iter()
                .for_each(|&w| self.spin[w] = self.spin[w].wrapping_add(t.rotate_left(4) as i32)),
            _ => self
                .spin
                .iter_mut()
                .for_each(|s| *s = s.wrapping_add(t.rotate_left(3) as i32)),
        }
    }

    /// `0x2726`: 16.16 vector divided by its integer length + 1.
    fn unit(v: [i32; 3]) -> [i32; 3] {
        let i = v.map(|c| (c >> 16) as f64);
        let len = (i[0] * i[0] + i[1] * i[1] + i[2] * i[2]).sqrt() as i32 + 1;
        v.map(|c| c / len)
    }

    fn diff(&self, p: usize, q: usize) -> [i32; 3] {
        [0, 1, 2].map(|k| self.atoms[p].pos[k].wrapping_sub(self.atoms[q].pos[k]))
    }

    /// Wheel axis for atom `i` (`0x0e69`): 0,1 steered (`0x1136`), 2,3 straight (`0x10b6`).
    fn axis(&self, i: usize) -> [i32; 3] {
        let (f1, f2) = (self.diff(4, 6), self.diff(5, 7));
        let fwd = Self::unit([0, 1, 2].map(|k| f1[k].wrapping_add(f2[k])));
        if i >= 2 {
            return fwd;
        }
        let side = Self::unit(self.diff(7, 6));
        let (s, c) = (
            (sin15(self.steer_now as u16) as i32) << 16,
            (cos15(self.steer_now as u16) as i32) << 16,
        );
        [0, 1, 2].map(|k| (hi(fwd[k], c) - hi(side[k], s)).wrapping_shl(1))
    }

    /// `0x0f67`: store the new spin, blended for some wheels depending on the drive mode.
    fn store_spin(&mut self, i: usize, new: i32) {
        let blend = if i < 2 {
            self.mode != 0
        } else {
            self.mode & 1 != 0
        };
        let old = self.spin[i];
        self.spin[i] = if blend {
            (old.wrapping_sub(new) >> 2).wrapping_add(new)
        } else {
            new
        };
    }
}

/// `0x532e`: height (integer units), slopes (Δh per cell along x, y) and ground colour at integer (x, y).
fn height(t: &Terrain, x: u16, y: u16) -> (i16, i16, i16, u8) {
    let at =
        |cx: u16, cy: u16| t.height[(cy as usize % SIZE) * SIZE + cx as usize % SIZE] as i16 * 16;
    let (cx, cy) = (x >> 8, y >> 8);
    let (mut fx, mut fy) = ((x & 0xff) as i16 >> 1, (y & 0xff) as i16 >> 1);
    let colour = t.colour[(cy as usize % SIZE) * SIZE + cx as usize % SIZE];
    let (base, dx, dy) = if fx + fy <= 0x80 {
        let h0 = at(cx, cy);
        (h0, at(cx + 1, cy) - h0, at(cx, cy + 1) - h0)
    } else {
        let h1 = at(cx + 1, cy + 1);
        fx = 0x80 - fx;
        fy = 0x80 - fy;
        // plane offsets go the other way; slopes stored negated (0x53cb, 0x53de)
        let (ex, ey) = (at(cx, cy + 1) - h1, at(cx + 1, cy) - h1);
        let h = h1 + ((ex as i32 * fx as i32 + ey as i32 * fy as i32) / 128) as i16;
        return (h, -ex, -ey, colour);
    };
    (
        base + ((dx as i32 * fx as i32 + dy as i32 * fy as i32) / 128) as i16,
        dx,
        dy,
        colour,
    )
}

impl Sim {
    pub fn new(terrain: Terrain) -> Sim {
        Sim {
            terrain,
            cars: Vec::new(),
            particles: Vec::new(),
            cfg: (0x0800, -0x1000),
            dig_everywhere: false,
            soft: vec![true; SIZE * SIZE],
        }
    }

    /// Swaps in a new map (hot reload): clears dirt particles and the soft-ground marks.
    pub fn set_terrain(&mut self, terrain: Terrain) {
        self.terrain = terrain;
        self.particles.clear();
        self.soft = vec![true; SIZE * SIZE];
    }

    /// Live dirt particle positions in world units.
    pub fn particle_positions(&self) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.particles.iter().map(|p| {
            [
                p.pos[0] as u32 as f32 / 65536.0,
                p.pos[1] as u32 as f32 / 65536.0,
                p.pos[2] as f32 / 65536.0,
            ]
        })
    }

    /// One 120 Hz timer tick (`0x5680`). `keys[i]` drives car `i`.
    pub fn tick(&mut self, keys: &[Keys]) {
        for i in 0..self.cars.len() {
            let car = &mut self.cars[i];
            car.input(keys.get(i).copied().unwrap_or_default());
            car.drive();
            self.springs(i);
            self.atoms(i);
        }
        self.step_particles();
    }

    /// Spring pass (`0x4e0a`), once per tick.
    fn springs(&mut self, ci: usize) {
        let cfg = self.cfg;
        let car = &mut self.cars[ci];
        for s in car.springs.iter_mut() {
            let (pa, pb) = (car.atoms[s.a], car.atoms[s.b]);
            let dpos = [0, 1, 2].map(|k| pa.pos[k].wrapping_sub(pb.pos[k]));
            let pred =
                [0, 1, 2].map(|k| dpos[k].wrapping_add(pa.vel[k].wrapping_sub(pb.vel[k])) as f64);
            let len = ((pred[0] * pred[0] + pred[1] * pred[1] + pred[2] * pred[2])
                .sqrt()
                .round() as i64 as i32)
                >> 10;

            if s.rest < 0 {
                s.rest = len as i16; // 0x5000
                s.aux = len as u16;
                continue;
            }
            let rest = s.rest as i32;
            let kind = s.kind; // `and cx, 0xff` never sets SF, so broken (bit 7) springs fall into the ≥2 branch
            let (bl, corr): (i32, i32) = if kind == 0 || kind > 1 {
                let bound = if len > s.max as i32 {
                    Some(s.max as i32)
                } else if len < s.min as i32 {
                    Some(s.min as i32)
                } else {
                    None
                };
                match (bound, kind) {
                    (Some(b), _) => {
                        // 0x4fce: halfway to the bound
                        let c = (b - len) >> 1;
                        if c == 0 {
                            continue;
                        }
                        s.aux = len as u16;
                        (len, c)
                    }
                    (None, 0) => continue,
                    (None, k) => {
                        // 0x4fa8: soft spring + damping on the previous length
                        let c = (rest - len) / ((k as i32) >> 1) + ((s.aux as i32 - len) >> 1);
                        s.aux = len as u16;
                        (len, c)
                    }
                }
            } else {
                let e = rest - len;
                if e == 0 {
                    continue;
                }
                if (e as i16) > cfg.0 {
                    let y = e >> 4; // 0x4fe3: yield
                    s.rest = (rest - y) as i16;
                    s.aux = s.rest as u16;
                    (len, y)
                } else if (e as i16) < cfg.1 {
                    s.kind |= 0x80; // 0x4ff7: break
                    continue;
                } else {
                    (len, e)
                }
            };
            // 0x4efe: velocity correction along the position delta
            let (num, den) = ((corr as i64) << 6, (bl as i64) << 6);
            if den == 0 {
                continue;
            }
            let cl = s.shift as u32 + 1;
            for k in 0..3 {
                let full = ((dpos[k] as i64 * num) / den) as i32;
                let part = full >> cl;
                car.atoms[s.a].vel[k] = car.atoms[s.a].vel[k].wrapping_add(part);
                car.atoms[s.b].vel[k] = car.atoms[s.b].vel[k].wrapping_sub(full - part);
            }
        }
    }

    /// Atom pass (`0x48db`): gravity, ground and wall contacts, integration.
    fn atoms(&mut self, ci: usize) {
        for i in 0..self.cars[ci].atoms.len() {
            let a = &mut self.cars[ci].atoms[i];
            a.vel[2] = a.vel[2].wrapping_sub(GRAVITY);
            let a = *a;
            let (x, y, z) = (
                (a.pos[0] >> 16) as u16,
                (a.pos[1] >> 16) as u16,
                (a.pos[2] >> 16) as i16,
            );
            let (h, sx, sy, colour) = height(&self.terrain, x, y);
            if h.wrapping_add(a.radius as i16).wrapping_sub(z) >= 0 {
                // 0x4a0d: surface damping, then contact along the terrain normal
                let k = DAMP[(colour >> 4) as usize];
                let at = &mut self.cars[ci].atoms[i];
                for v in &mut at.vel {
                    *v -= *v >> k;
                }
                let (ax_, ay_) = (atan2(sx, 0x100), atan2(sy, 0x100));
                let n = [sin15(ax_), cos15(ax_), sin15(ay_), cos15(ay_)].map(|v| (v as i32) << 16);
                self.contact(ci, i, n, colour);
            }
            let r = a.radius;
            let (wx, wy) = ((a.pos[0] >> 16) as u16, (a.pos[1] >> 16) as u16);
            let walls: [(bool, [i32; 4]); 4] = [
                (wx.wrapping_sub(r) < 0x80, [i32::MIN, 0, 0, 0x7f00 << 16]),
                (wy.wrapping_sub(r) < 0x80, [0, 0x7fff << 16, i32::MIN, 0]),
                (
                    wx.wrapping_add(r) > 0xfe80,
                    [0x7fff << 16, 0, 0, 0x7fff << 16],
                ),
                (
                    wy.wrapping_add(r) > 0xfe80,
                    [0, 0x7fff << 16, 0x7fff << 16, 0],
                ),
            ];
            for (hit, n) in walls {
                if hit {
                    self.contact(ci, i, n, 0);
                }
            }
            let at = &mut self.cars[ci].atoms[i];
            for k in 0..3 {
                at.pos[k] = at.pos[k].wrapping_add(at.vel[k]);
            }
        }
    }

    /// `0x4a71`: bounce off the plane, then friction (wheel or body).
    fn contact(&mut self, ci: usize, i: usize, n: [i32; 4], colour: u8) {
        let [na, nb, nc, nd] = n;
        let at = &mut self.cars[ci].atoms[i];
        let vn = q31(q31(at.vel[2], nb), nd) - q31(at.vel[0], na) - q31(at.vel[1], nc);
        if vn >= 0 {
            return;
        }
        let ecx = vn + (vn >> 2);
        let dx = hi(na, ecx);
        at.vel[0] += dx;
        at.pos[0] += dx.wrapping_shl(1);
        let dy = hi(nc, ecx);
        at.vel[1] += dy;
        at.pos[1] += dy.wrapping_shl(1);
        let dz = hi(q31(ecx, nb), nd);
        at.vel[2] -= dz;
        at.pos[2] -= dz.wrapping_shl(1);

        let c = Contact { n, vn, colour };
        let at = *at;
        let tx = q31(at.vel[0], nb) + q31(at.vel[2], na);
        let ty = q31(at.vel[1], nd) + q31(at.vel[2], nc);
        if at.kind == 0 || at.kind == 0xffff || i >= 4 {
            self.body_friction(ci, i, &c, tx, ty);
        } else {
            self.wheel_friction(ci, i, &c, tx, ty);
        }
    }

    /// `0x4c68`: true if the surface can hold this slip; also returns grip/2.
    fn grips(c: &Contact, tx: i32, ty: i32) -> (bool, i32) {
        let s = (c.colour >> 4) as usize;
        let slip = tx.wrapping_abs().wrapping_add(ty.wrapping_abs());
        let budget = (-c.vn).min(MAX_LOAD[s]) / GRIP[s];
        (budget > slip >> 6, GRIP[s] >> 1)
    }

    fn body_friction(&mut self, ci: usize, i: usize, c: &Contact, tx: i32, ty: i32) {
        let (hold, half) = Self::grips(c, tx, ty);
        let (ex, ey) = if hold {
            (tx, ty)
        } else {
            self.spawn_dirt(ci, i, c, tx, ty);
            (tx / half, ty / half)
        };
        self.apply_slip(ci, i, c, ex, ey);
    }

    fn wheel_friction(&mut self, ci: usize, i: usize, c: &Contact, tx: i32, ty: i32) {
        let [na, nb, nc, nd] = c.n;
        let axis = self.cars[ci].axis(i);
        let mut neg_spin = -self.cars[ci].spin[i]; // [0xe9da]
        // 0x4c26: wheel heading in the terrain plane
        let p1 = -((hi(axis[0] >> 4, nb) as i16).wrapping_add(hi(axis[2] >> 4, na) as i16));
        let p2 = (hi(axis[1] >> 4, nd) as i16).wrapping_add(hi(axis[2] >> 4, nc) as i16);
        let th = atan2(p1, p2);
        let (s, co) = ((sin15(th) as i32) << 16, (cos15(th) as i32) << 16);
        let d = neg_spin.wrapping_shl(1);
        let (mut ex, mut ey) = (tx + hi(d, s), ty - hi(d, co));

        // 0x4d0e
        let (hold, half) = Self::grips(c, ex, ey);
        if !hold {
            self.spawn_dirt(ci, i, c, ex, ey);
            ex /= half;
            ey /= half;
        }
        let roll = hi(ex, s) - hi(ey, co);
        neg_spin -= roll;
        ex += hi(roll, s);
        ey -= hi(roll, co);
        self.apply_slip(ci, i, c, ex, ey);
        self.cars[ci].store_spin(i, -neg_spin);
    }

    /// Removes slip from the atom (`0x4cf1` / `0x4d74`).
    fn apply_slip(&mut self, ci: usize, i: usize, c: &Contact, ex: i32, ey: i32) {
        let at = &mut self.cars[ci].atoms[i];
        let dx = hi(ex, c.n[1]);
        at.vel[0] -= dx;
        at.pos[0] -= dx;
        let dy = hi(ey, c.n[3]);
        at.vel[1] -= dy;
        at.pos[1] -= dy;
    }

    /// `0x4d96`: a dirt particle flies off a sliding contact.
    fn spawn_dirt(&mut self, ci: usize, i: usize, c: &Contact, tx: i32, ty: i32) {
        if self.particles.len() >= MAX_PARTICLES {
            return;
        }
        let at = self.cars[ci].atoms[i];
        let cell = ((at.pos[1] as u32 >> 24) as usize) * SIZE + (at.pos[0] as u32 >> 24) as usize;
        let colour = if self.dig_everywhere && self.soft[cell] {
            DIG_COLOUR as u16
        } else {
            c.colour as u16
        };
        self.particles.push(Particle {
            pos: [at.pos[0], at.pos[1], at.pos[2] - ((at.radius as i32) << 16)],
            vel: [q31(tx, c.n[1]), q31(ty, c.n[3]), at.vel[2]],
            life: 0x64,
            colour,
        });
    }

    /// `0x0bb5`: move particles; heavy ones (colour 0x0f) fall and dig where they land.
    fn step_particles(&mut self) {
        let mut j = 0;
        while j < self.particles.len() {
            let p = &mut self.particles[j];
            p.life -= 2;
            if p.life < 0 {
                self.particles.swap_remove(j);
                continue;
            }
            for k in 0..3 {
                p.pos[k] = p.pos[k].wrapping_add(p.vel[k]);
            }
            if p.colour == DIG_COLOUR as u16 {
                let (x, y, z) = (
                    (p.pos[0] >> 16) as u16,
                    (p.pos[1] >> 16) as u16,
                    (p.pos[2] >> 16) as i16,
                );
                if height(&self.terrain, x, y).0.wrapping_sub(z) >= 0 {
                    let cell = |x: u16, y: u16| (y as usize >> 8) * SIZE + (x as usize >> 8);
                    let col = &mut self.terrain.colour[cell(x, y)];
                    if *col & 0xf != 0 {
                        *col -= 1; // darker mark
                    }
                    self.soft[cell(x, y)] = false;
                    let h =
                        &mut self.terrain.height[cell(x.wrapping_add(0x80), y.wrapping_add(0x80))];
                    *h = h.saturating_sub(1); // the dent; the asm's `dec byte` wraps 0 → 255 (a spike), never hit in the shipped game
                    p.colour = 1;
                    p.vel = [0, 0, 0x2710];
                } else {
                    p.vel[2] -= GRAVITY >> 1;
                }
            }
            j += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim() -> Sim {
        let read = |f: &str| crate::assets::read(f).unwrap();
        let mut sim = Sim::new(
            Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap(),
        );
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        sim.cars
            .push(SimCar::spawn(&car, &sim.terrain, 0x8000, 0x7a00));
        sim
    }

    #[test]
    fn trig_matches_original_conventions() {
        assert_eq!(sin15(0), 0);
        assert_eq!(sin15(0x4000), 32767);
        assert_eq!(sin15(0xc000), -32767);
        assert_eq!(cos15(0), 32767);
        assert_eq!(atan2(0, 5), 0);
        assert_eq!(atan2(5, 0), 0x4000);
        assert_eq!(atan2(5, 5), 0x2000);
    }

    #[test]
    fn car_settles_and_drives() {
        let mut s = sim();
        for _ in 0..TICK_HZ * 3 {
            s.tick(&[Keys::default()]);
        }
        let rest = s.cars[0].centre();
        let ground = height(&s.terrain, rest[0] as u16, rest[1] as u16).0 as f32;
        assert!(
            (rest[2] - ground).abs() < 120.0,
            "car should rest on the ground: z={} ground={ground}",
            rest[2]
        );
        assert!(
            s.cars[0].springs.iter().all(|sp| sp.kind & 0x80 == 0),
            "nothing breaks at rest"
        );

        for _ in 0..TICK_HZ * 3 {
            s.tick(&[Keys {
                throttle: true,
                ..Keys::default()
            }]);
        }
        let moved = s.cars[0].centre();
        let dist = ((moved[0] - rest[0]).powi(2) + (moved[1] - rest[1]).powi(2)).sqrt();
        assert!(dist > 100.0, "throttle should move the car, moved {dist}");
    }

    #[test]
    fn digs_only_diggable_ground() {
        for soft in [false, true] {
            let mut s = sim();
            s.dig_everywhere = soft;
            let before = s.terrain.height.clone();
            let gas = Keys {
                throttle: true,
                left: true,
                ..Keys::default()
            };
            for t in 0..TICK_HZ * 6 {
                s.tick(&[if t < TICK_HZ { Keys::default() } else { gas }]);
            }
            let dents = s
                .terrain
                .height
                .iter()
                .zip(&before)
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(dents > 0, soft, "soft={soft} dents={dents}");
            assert!(
                s.terrain.height.iter().zip(&before).all(|(a, b)| a <= b),
                "digging only lowers"
            );
        }
    }

    /// Same inputs, same run: input replays (#6) rely on it. Within one build and platform only;
    /// the sin table and atan2 go through f64 libm, which could differ elsewhere.
    #[test]
    fn same_inputs_same_run() {
        let run = || {
            let mut s = sim();
            for t in 0..TICK_HZ * 8 {
                s.tick(&[Keys {
                    throttle: t % 300 < 200,
                    left: t % 170 < 40,
                    right: t % 230 > 180,
                    brake: t % 500 > 470,
                    ..Keys::default()
                }]);
            }
            (s.cars[0].atom_positions(), s.terrain.height)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn push_and_drag_change_speed() {
        let speed_after = |pad: &dyn Fn(&mut SimCar)| {
            let mut s = sim();
            let gas = Keys {
                throttle: true,
                ..Keys::default()
            };
            for t in 0..TICK_HZ * 3 {
                s.tick(&[gas]);
                if t > TICK_HZ {
                    pad(&mut s.cars[0]);
                }
            }
            let a = s.cars[0].centre();
            s.tick(&[gas]);
            let b = s.cars[0].centre();
            (b[0] - a[0]).hypot(b[1] - a[1])
        };
        let plain = speed_after(&|_| {});
        let boosted = speed_after(&|c| c.push(0x2000));
        let slowed = speed_after(&|c| c.drag(4));
        assert!(boosted > plain * 1.2, "boost {boosted} vs {plain}");
        assert!(slowed < plain * 0.8, "slow {slowed} vs {plain}");
    }

    #[test]
    fn upside_down_only_on_the_roof() {
        let flat = || Terrain {
            height: vec![40; SIZE * SIZE],
            colour: vec![0x60; SIZE * SIZE],
            palette: [[0; 3]; 256],
        };
        for n in 1..=5 {
            let mut car =
                crate::car::parse(&crate::assets::read(&format!("CAR{n}.DAT")).unwrap()).unwrap();
            let s = Sim::new(flat());
            for h in [0.0, 1.0, 2.5] {
                let c = SimCar::spawn_facing(&car, &s.terrain, 0x8000, 0x8000, h);
                assert!(!c.upside_down(), "CAR{n} upright, heading {h}");
            }
            // roll 180° about the length axis: x → −x, z → −z
            car.atoms.iter_mut().for_each(|a| {
                a.pos[0] = -a.pos[0];
                a.pos[2] = -a.pos[2];
            });
            let c = SimCar::spawn(&car, &s.terrain, 0x8000, 0x8000);
            assert!(c.upside_down(), "CAR{n} on its roof");
        }
    }

    #[test]
    fn drives_straight_on_flat_ground() {
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        let flat = Terrain {
            height: vec![40; SIZE * SIZE],
            colour: vec![0x60; SIZE * SIZE],
            palette: [[0; 3]; 256],
        };
        let mut s = Sim::new(flat);
        s.cars.push(SimCar::spawn(&car, &s.terrain, 0x8000, 0xc000));
        let heading = |s: &Sim| {
            let a = s.cars[0].atom_positions();
            (a[6][0] + a[7][0] - a[4][0] - a[5][0])
                .atan2(a[6][1] + a[7][1] - a[4][1] - a[5][1])
                .to_degrees()
        };
        for _ in 0..TICK_HZ {
            s.tick(&[Keys::default()]);
        }
        let h0 = heading(&s);
        for _ in 0..TICK_HZ * 8 {
            s.tick(&[Keys {
                throttle: true,
                ..Keys::default()
            }]);
        }
        assert!(
            (heading(&s) - h0).abs() < 5.0,
            "heading drifted {h0} -> {}",
            heading(&s)
        );
    }
}
