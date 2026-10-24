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

/// Quarter-wave Q15 sine table, 1024 steps (DS 0xd10c): round(sin(i·π/2048)·32768), capped.
const SIN_QUARTER: [i16; 1025] = [
    0, 50, 101, 151, 201, 251, 302, 352, 402, 452, 503, 553, 603, 653, 704, 754, 804, 854, 905,
    955, 1005, 1055, 1106, 1156, 1206, 1256, 1307, 1357, 1407, 1457, 1507, 1558, 1608, 1658, 1708,
    1758, 1809, 1859, 1909, 1959, 2009, 2060, 2110, 2160, 2210, 2260, 2310, 2360, 2411, 2461, 2511,
    2561, 2611, 2661, 2711, 2761, 2811, 2861, 2912, 2962, 3012, 3062, 3112, 3162, 3212, 3262, 3312,
    3362, 3412, 3462, 3512, 3562, 3612, 3662, 3712, 3762, 3812, 3861, 3911, 3961, 4011, 4061, 4111,
    4161, 4211, 4260, 4310, 4360, 4410, 4460, 4510, 4559, 4609, 4659, 4709, 4758, 4808, 4858, 4907,
    4957, 5007, 5057, 5106, 5156, 5205, 5255, 5305, 5354, 5404, 5453, 5503, 5553, 5602, 5652, 5701,
    5751, 5800, 5850, 5899, 5948, 5998, 6047, 6097, 6146, 6195, 6245, 6294, 6343, 6393, 6442, 6491,
    6541, 6590, 6639, 6688, 6737, 6787, 6836, 6885, 6934, 6983, 7032, 7081, 7130, 7180, 7229, 7278,
    7327, 7376, 7425, 7473, 7522, 7571, 7620, 7669, 7718, 7767, 7816, 7864, 7913, 7962, 8011, 8059,
    8108, 8157, 8206, 8254, 8303, 8351, 8400, 8449, 8497, 8546, 8594, 8643, 8691, 8740, 8788, 8836,
    8885, 8933, 8982, 9030, 9078, 9127, 9175, 9223, 9271, 9319, 9368, 9416, 9464, 9512, 9560, 9608,
    9656, 9704, 9752, 9800, 9848, 9896, 9944, 9992, 10040, 10088, 10135, 10183, 10231, 10279,
    10326, 10374, 10422, 10469, 10517, 10565, 10612, 10660, 10707, 10755, 10802, 10850, 10897,
    10945, 10992, 11039, 11087, 11134, 11181, 11228, 11276, 11323, 11370, 11417, 11464, 11511,
    11558, 11605, 11652, 11699, 11746, 11793, 11840, 11887, 11934, 11980, 12027, 12074, 12121,
    12167, 12214, 12261, 12307, 12354, 12400, 12447, 12493, 12540, 12586, 12633, 12679, 12725,
    12772, 12818, 12864, 12910, 12957, 13003, 13049, 13095, 13141, 13187, 13233, 13279, 13325,
    13371, 13417, 13463, 13508, 13554, 13600, 13646, 13691, 13737, 13783, 13828, 13874, 13919,
    13965, 14010, 14056, 14101, 14146, 14192, 14237, 14282, 14327, 14373, 14418, 14463, 14508,
    14553, 14598, 14643, 14688, 14733, 14778, 14823, 14867, 14912, 14957, 15002, 15046, 15091,
    15136, 15180, 15225, 15269, 15314, 15358, 15402, 15447, 15491, 15535, 15580, 15624, 15668,
    15712, 15756, 15800, 15844, 15888, 15932, 15976, 16020, 16064, 16108, 16151, 16195, 16239,
    16282, 16326, 16369, 16413, 16456, 16500, 16543, 16587, 16630, 16673, 16717, 16760, 16803,
    16846, 16889, 16932, 16975, 17018, 17061, 17104, 17147, 17190, 17233, 17275, 17318, 17361,
    17403, 17446, 17488, 17531, 17573, 17616, 17658, 17700, 17743, 17785, 17827, 17869, 17911,
    17953, 17995, 18037, 18079, 18121, 18163, 18205, 18247, 18288, 18330, 18372, 18413, 18455,
    18496, 18538, 18579, 18621, 18662, 18703, 18745, 18786, 18827, 18868, 18909, 18950, 18991,
    19032, 19073, 19114, 19155, 19195, 19236, 19277, 19317, 19358, 19399, 19439, 19479, 19520,
    19560, 19601, 19641, 19681, 19721, 19761, 19801, 19841, 19881, 19921, 19961, 20001, 20041,
    20081, 20120, 20160, 20200, 20239, 20279, 20318, 20357, 20397, 20436, 20475, 20515, 20554,
    20593, 20632, 20671, 20710, 20749, 20788, 20827, 20865, 20904, 20943, 20981, 21020, 21059,
    21097, 21136, 21174, 21212, 21251, 21289, 21327, 21365, 21403, 21441, 21479, 21517, 21555,
    21593, 21631, 21668, 21706, 21744, 21781, 21819, 21856, 21894, 21931, 21968, 22006, 22043,
    22080, 22117, 22154, 22191, 22228, 22265, 22302, 22339, 22375, 22412, 22449, 22485, 22522,
    22558, 22595, 22631, 22668, 22704, 22740, 22776, 22812, 22848, 22884, 22920, 22956, 22992,
    23028, 23064, 23099, 23135, 23170, 23206, 23241, 23277, 23312, 23348, 23383, 23418, 23453,
    23488, 23523, 23558, 23593, 23628, 23663, 23697, 23732, 23767, 23801, 23836, 23870, 23905,
    23939, 23973, 24008, 24042, 24076, 24110, 24144, 24178, 24212, 24246, 24279, 24313, 24347,
    24380, 24414, 24448, 24481, 24514, 24548, 24581, 24614, 24647, 24680, 24713, 24746, 24779,
    24812, 24845, 24878, 24910, 24943, 24976, 25008, 25041, 25073, 25105, 25138, 25170, 25202,
    25234, 25266, 25298, 25330, 25362, 25394, 25425, 25457, 25489, 25520, 25552, 25583, 25615,
    25646, 25677, 25708, 25739, 25771, 25802, 25833, 25863, 25894, 25925, 25956, 25986, 26017,
    26048, 26078, 26108, 26139, 26169, 26199, 26229, 26259, 26290, 26320, 26349, 26379, 26409,
    26439, 26468, 26498, 26528, 26557, 26586, 26616, 26645, 26674, 26704, 26733, 26762, 26791,
    26820, 26848, 26877, 26906, 26935, 26963, 26992, 27020, 27049, 27077, 27105, 27133, 27162,
    27190, 27218, 27246, 27273, 27301, 27329, 27357, 27384, 27412, 27440, 27467, 27494, 27522,
    27549, 27576, 27603, 27630, 27657, 27684, 27711, 27738, 27765, 27791, 27818, 27844, 27871,
    27897, 27924, 27950, 27976, 28002, 28028, 28054, 28080, 28106, 28132, 28158, 28183, 28209,
    28234, 28260, 28285, 28311, 28336, 28361, 28386, 28411, 28436, 28461, 28486, 28511, 28536,
    28560, 28585, 28610, 28634, 28658, 28683, 28707, 28731, 28755, 28779, 28803, 28827, 28851,
    28875, 28899, 28922, 28946, 28970, 28993, 29016, 29040, 29063, 29086, 29109, 29132, 29155,
    29178, 29201, 29224, 29247, 29269, 29292, 29314, 29337, 29359, 29381, 29404, 29426, 29448,
    29470, 29492, 29514, 29535, 29557, 29579, 29600, 29622, 29643, 29665, 29686, 29707, 29729,
    29750, 29771, 29792, 29813, 29833, 29854, 29875, 29895, 29916, 29936, 29957, 29977, 29997,
    30018, 30038, 30058, 30078, 30098, 30118, 30137, 30157, 30177, 30196, 30216, 30235, 30254,
    30274, 30293, 30312, 30331, 30350, 30369, 30388, 30407, 30425, 30444, 30462, 30481, 30499,
    30518, 30536, 30554, 30572, 30590, 30608, 30626, 30644, 30662, 30680, 30697, 30715, 30732,
    30750, 30767, 30784, 30801, 30819, 30836, 30853, 30869, 30886, 30903, 30920, 30936, 30953,
    30969, 30986, 31002, 31018, 31034, 31050, 31067, 31082, 31098, 31114, 31130, 31146, 31161,
    31177, 31192, 31207, 31223, 31238, 31253, 31268, 31283, 31298, 31313, 31328, 31342, 31357,
    31372, 31386, 31400, 31415, 31429, 31443, 31457, 31471, 31485, 31499, 31513, 31527, 31540,
    31554, 31568, 31581, 31594, 31608, 31621, 31634, 31647, 31660, 31673, 31686, 31699, 31711,
    31724, 31737, 31749, 31761, 31774, 31786, 31798, 31810, 31822, 31834, 31846, 31858, 31870,
    31881, 31893, 31904, 31916, 31927, 31938, 31950, 31961, 31972, 31983, 31994, 32005, 32015,
    32026, 32037, 32047, 32058, 32068, 32078, 32088, 32099, 32109, 32119, 32129, 32138, 32148,
    32158, 32167, 32177, 32186, 32196, 32205, 32214, 32224, 32233, 32242, 32251, 32259, 32268,
    32277, 32286, 32294, 32303, 32311, 32319, 32328, 32336, 32344, 32352, 32360, 32368, 32376,
    32383, 32391, 32398, 32406, 32413, 32421, 32428, 32435, 32442, 32449, 32456, 32463, 32470,
    32477, 32483, 32490, 32496, 32503, 32509, 32515, 32522, 32528, 32534, 32540, 32546, 32551,
    32557, 32563, 32568, 32574, 32579, 32585, 32590, 32595, 32600, 32605, 32610, 32615, 32620,
    32625, 32629, 32634, 32638, 32643, 32647, 32651, 32656, 32660, 32664, 32668, 32672, 32675,
    32679, 32683, 32686, 32690, 32693, 32697, 32700, 32703, 32706, 32709, 32712, 32715, 32718,
    32721, 32723, 32726, 32729, 32731, 32733, 32736, 32738, 32740, 32742, 32744, 32746, 32748,
    32749, 32751, 32753, 32754, 32756, 32757, 32758, 32759, 32760, 32761, 32762, 32763, 32764,
    32765, 32766, 32766, 32767, 32767, 32767, 32767, 32767, 32767, 32767,
];

/// Q15 sine of a 0x10000-per-turn angle, like `0x2aad` (table at DS 0xd10c). A table rather
/// than libm `sin`, so every platform gets the same numbers (replays depend on it).
fn sin15(angle: u16) -> i16 {
    let quarter = |i: u16| SIN_QUARTER[i as usize];
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
/// Direction of the world vector (dx, dy) as a heading: 0x10000 per turn, clockwise from
/// map-up (−y), the convention of `SimCar::spawn_facing`.
pub fn heading_of(dx: i32, dy: i32) -> u16 {
    angle_of(dx as i64, -(dy as i64))
}

/// `0x2b08`: angle of (x = bx, y = ax) in 0x10000-per-turn units.
fn atan2(ax: i16, bx: i16) -> u16 {
    angle_of(ax as i64, bx as i64)
}

/// atan(2⁻ⁱ) in 2³²-per-turn units, for `angle_of`'s CORDIC.
const ATAN_STEPS: [i64; 31] = [
    536870912, 316933406, 167458907, 85004756, 42667331, 21354465, 10679838, 5340245, 2670163,
    1335087, 667544, 333772, 166886, 83443, 41722, 20861, 10430, 5215, 2608, 1304, 652, 326, 163,
    81, 41, 20, 10, 5, 3, 1, 1,
];

/// atan2(y, x) in 0x10000-per-turn units, rounded, in integers only: platform libm atan2s may
/// differ in the last bit, and networked play needs every machine to compute the same tick.
/// CORDIC to ~10⁻⁹ of a turn, so it rounds like the f64 atan2 it replaces (the physics is
/// sensitive to it: even 1-unit differences on a few percent of calls change how cars collide).
fn angle_of(y: i64, x: i64) -> u16 {
    if x == 0 && y == 0 {
        return 0;
    }
    // into the right half-plane (CORDIC converges within ±99°), scaled up to ~2⁴⁰
    let (mut x, mut y, mut z) = if x < 0 {
        (-x, -y, 1i64 << 31)
    } else {
        (x, y, 0)
    };
    let shift = 40 - (64 - x.abs().max(y.abs()).leading_zeros() as i32);
    if shift > 0 {
        (x, y) = (x << shift, y << shift);
    }
    // rotate (x, y) onto the x axis, adding up the angles turned
    for (i, step) in ATAN_STEPS.iter().enumerate() {
        let (dx, dy) = (y >> i, x >> i);
        if y > 0 {
            (x, y, z) = (x + dx, y - dy, z + step);
        } else {
            (x, y, z) = (x - dx, y + dy, z - step);
        }
    }
    ((z + (1 << 15)) >> 16) as u16
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
    /// Total yield so far (×64 units, like `rest`): how bent this spring is. Not in the original.
    bent: u16,
    /// `rest` as built (measured on the first tick when the file leaves it to that), what
    /// `mend` pulls a bent or broken spring back to. Not in the original.
    built: i16,
}

#[derive(Clone, Copy, Default)]
pub struct Keys {
    pub throttle: bool,
    pub reverse: bool,
    pub left: bool,
    pub right: bool,
    pub brake: bool,
}

#[derive(Clone)]
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
    /// Gradual repair's budget (#58), in `REPAIR_FULL` units: spent by `mend`, refilled every tick.
    budget: u32,
    /// `spans` as built, for `misshapen`.
    shape: Vec<f64>,
    /// `mend` ran this tick: the springs hold firm instead of bending or breaking, so a car
    /// mended on its roof or against a wall isn't crushed again by its own weight.
    mending: bool,
}

/// The budget that repairs a car from damage 1.0 to as built.
pub const REPAIR_FULL: u32 = 1 << 16;
/// A full budget bar: one whole repair (ponytail: try `REPAIR_FULL / 2` for a harsher game).
pub const REPAIR_CAP: u32 = REPAIR_FULL;
/// Refill per tick: empty to full in ~90 s.
const REPAIR_RECHARGE: u32 = REPAIR_CAP / (90 * TICK_HZ);
/// How far `mend` moves a spring's rest length per tick (×64 units): an 8-unit dent in ~1 s, a
/// wreck in 5–10 s of holding F.
const MEND_STEP: i16 = 4;
/// While mending, a rigid spring corrects at most this much of its error (×64) per tick: what a
/// yielding spring gives (`cfg.0 >> 4`).
const MEND_PUSH: i32 = 0x80;
/// A rigid spring bent this far (×64) or broken counts fully towards `damage`.
const FULL_HURT: i64 = 8 * 64;

#[derive(Clone, Copy)]
struct Particle {
    pos: [i32; 3],
    vel: [i32; 3],
    life: i16,
    colour: u16,
}

/// A tree trunk or rock (#49): a vertical cylinder standing at world (x, y), `r` units around,
/// reaching `h` above the ground there. Integers, so every machine collides the same.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trunk {
    pub x: u16,
    pub y: u16,
    pub r: u16,
    pub h: u16,
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
    /// Soft dirt (CHECK.PCX): cells that dig on every pass, down to `rut_floor`. Not in the original.
    soft_dirt: Vec<bool>,
    rut_floor: Vec<u8>,
    /// Scenery colliders (OBJECTS.PCX). Not in the original.
    trunks: Vec<Trunk>,
    /// Hard landings so far (vn < −6.0, where `0x4a71` calls the thud sound `0x5864`).
    pub impacts: u32,
    /// Repair (`run::REPAIR`) mends while held, from a budget (#58), instead of a fresh car at once.
    pub gradual_repair: bool,
}

/// How deep soft dirt ruts can get, in MAP height steps (16 world units each).
pub const MAX_RUT: u8 = 6;

impl SimCar {
    /// Places a parsed car at world (x, y) like `0x2454`: z = ground + 100, facing the map's −y.
    pub fn spawn(car: &Car, terrain: &Terrain, x: u16, y: u16) -> SimCar {
        Self::spawn_facing(car, terrain, x, y, 0)
    }

    /// Like `spawn`, turned `heading` clockwise on the map image (0x10000 per turn, 0 = −y, the
    /// original's facing). Integer rotation through the sine table: the same on every platform.
    pub fn spawn_facing(car: &Car, terrain: &Terrain, x: u16, y: u16, heading: u16) -> SimCar {
        let ground = height(terrain, x, y).0 as i32 + 0x64;
        let fx = |v: f32| (v * 65536.0) as i64;
        // the table's ±32767 is ±1.0 here, so the four axis directions rotate exactly
        let unit = |v: i16| {
            if v.abs() == 32767 {
                v.signum() as i64 * 32768
            } else {
                v as i64
            }
        };
        let (sin, cos) = (unit(sin15(heading)), unit(cos15(heading)));
        let atoms = car
            .atoms
            .iter()
            .map(|a| Atom {
                pos: [
                    (((fx(a.pos[0]) * cos - fx(a.pos[1]) * sin) >> 15) as i32)
                        .wrapping_add(((x as u32) << 16) as i32),
                    (((fx(a.pos[0]) * sin + fx(a.pos[1]) * cos) >> 15) as i32)
                        .wrapping_add(((y as u32) << 16) as i32),
                    fx(a.pos[2]) as i32 + (ground << 16),
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
                bent: 0,
                built: s.rest.map_or(-1, |r| (r * 64.0) as i16),
            })
            .collect();
        let mode = match car.drive {
            Drive::Rear => 0,
            Drive::Front => 1,
            Drive::All => 2,
        };
        let mut c = SimCar {
            atoms,
            springs,
            mode,
            steer: 0,
            throttle: 0,
            brake: false,
            steer_now: 0,
            throttle_now: 0,
            spin: [0; 4],
            budget: REPAIR_CAP,
            shape: vec![],
            mending: false,
        };
        c.shape = c.spans();
        c
    }

    /// How far (world units) the body is from its built shape: the largest change in distance
    /// between any two body atoms. Right spring lengths can still leave it folded inside out
    /// after a big wreck; then gradual repair finishes by putting it back in shape (`run`).
    pub fn misshapen(&self) -> f64 {
        let now = self.spans();
        self.shape
            .iter()
            .zip(&now)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
            / 65536.0
    }

    /// The distance (16.16) between each pair of body atoms, wheels left out (the suspension
    /// moves them): unlike positions, the same however the car lies, so a fold shows in it.
    fn spans(&self) -> Vec<f64> {
        let body: Vec<&Atom> = self
            .atoms
            .iter()
            .filter(|a| !matches!(a.kind, 1 | 2))
            .collect();
        let mut out = vec![];
        for (i, a) in body.iter().enumerate() {
            for b in &body[i + 1..] {
                let d = [0, 1, 2].map(|k| a.pos[k].wrapping_sub(b.pos[k]) as f64);
                out.push((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt());
            }
        }
        out
    }

    /// The car's oriented bounding box for car–car contact: centre (an offset from atom 0), unit
    /// axes forward / right / up, and half-extents, in 16.16 units. The original meant to use a
    /// hull of atom triangles (`0x5091`, table `[0xec1b]`), but the shipped game never fills it.
    fn bounds(&self) -> ([f64; 3], [[f64; 3]; 3], [f64; 3]) {
        let d = |i: usize| {
            [0, 1, 2].map(|k| self.atoms[i].pos[k].wrapping_sub(self.atoms[0].pos[k]) as f64)
        };
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let add = |a: [f64; 3], b: [f64; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        let unit = |v: [f64; 3]| {
            let l = dot(v, v).sqrt().max(1.0);
            v.map(|c| c / l)
        };
        let n = self.atoms.len();
        let centre = (0..n).map(d).fold([0.0; 3], add).map(|c| c / n as f64);
        // axes from the wheels (0, 1 front, 2, 3 rear), which share a plane; body atoms don't
        let fwd = unit(sub(add(d(0), d(1)), add(d(2), d(3))));
        let across = sub(d(1), d(0));
        let side = unit(sub(across, fwd.map(|c| c * dot(across, fwd))));
        let up = [
            fwd[1] * side[2] - fwd[2] * side[1],
            fwd[2] * side[0] - fwd[0] * side[2],
            fwd[0] * side[1] - fwd[1] * side[0],
        ];
        let axes = [fwd, side, up];
        let ext = axes.map(|ax| {
            (0..n)
                .map(|i| dot(sub(d(i), centre), ax).abs())
                .fold(0.0, f64::max)
        });
        (centre, axes, ext)
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

    /// Which way the car points (front axle minus rear), as a `spawn_facing` heading.
    pub fn heading(&self) -> u16 {
        let d =
            |i: usize, j: usize, k: usize| self.atoms[i].pos[k].wrapping_sub(self.atoms[j].pos[k]);
        let (dx, dy) = (
            (d(6, 4, 0) >> 1).wrapping_add(d(7, 5, 0) >> 1),
            (d(6, 4, 1) >> 1).wrapping_add(d(7, 5, 1) >> 1),
        );
        heading_of(dx, dy)
    }

    /// Current steering angle in radians (positive = right).
    pub fn steer_angle(&self) -> f32 {
        self.steer_now as f32 / 65536.0 * std::f32::consts::TAU
    }

    /// Engine revs as the original's sound code reads them (main loop, `0x02cf`): the driven
    /// wheels' spin, each pair summed and `sar 14`: rear (RWD), front (FWD) or the two averaged (4WD).
    pub fn revs(&self) -> i32 {
        let pair = |a: usize, b: usize| self.spin[a].wrapping_add(self.spin[b]) >> 14;
        match self.mode {
            0 => pair(2, 3),
            1 => pair(0, 1),
            _ => (pair(0, 1) + pair(2, 3)) / 2,
        }
    }

    /// The throttle control word (`+0x0c`): ramps towards ±0x2000 while the pedal is held.
    pub fn throttle_word(&self) -> i16 {
        self.throttle
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
        let len = (fwd[0] * fwd[0] + fwd[1] * fwd[1]).sqrt().max(1.0); // not libm hypot: IEEE sqrt is exact everywhere
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

    /// Upright again at (x, y) facing `heading`, keeping the damage: the bent and broken springs
    /// carry over, so the dents pull back into shape within a few ticks. (A fresh `spawn` repairs.)
    pub fn respawned(&self, car: &Car, terrain: &Terrain, x: u16, y: u16, heading: u16) -> SimCar {
        let mut c = SimCar::spawn_facing(car, terrain, x, y, heading);
        c.springs.clone_from(&self.springs);
        c.budget = self.budget;
        c
    }

    /// 0.0 (as built) to 1.0: rigid springs bent by 8 units or more, or broken, count fully.
    pub fn damage(&self) -> f32 {
        let (hurt, most) = self.hurt();
        hurt as f32 / most as f32
    }

    /// (how hurt, the most it can be): `FULL_HURT` per broken rigid spring, its bend up to that
    /// for the rest. Integers, so the repair budget spends the same on every machine.
    fn hurt(&self) -> (i64, i64) {
        let rigid = self.springs.iter().filter(|s| s.kind & 0x7f == 1);
        let hurt = rigid
            .clone()
            .map(|s| {
                if s.kind & 0x80 != 0 {
                    FULL_HURT
                } else {
                    (s.bent as i64).min(FULL_HURT)
                }
            })
            .sum();
        (hurt, rigid.count().max(1) as i64 * FULL_HURT)
    }

    /// Raise the whole car by `dz` world units, for tests that drop it.
    #[cfg(test)]
    pub(crate) fn lift(&mut self, dz: i32) {
        self.atoms.iter_mut().for_each(|a| a.pos[2] += dz << 16);
    }

    /// Empties the repair budget (a full repair at once took it all).
    pub fn spend_all(&mut self) {
        self.budget = 0;
    }

    /// Whether every spring is as built (gradual repair has nothing left to mend).
    pub fn whole(&self) -> bool {
        self.hurt().0 == 0
    }

    /// Gradual repair's budget, 0 to `REPAIR_CAP` (`REPAIR_FULL` = one whole repair).
    pub fn budget(&self) -> u32 {
        self.budget
    }

    /// One tick of gradual repair (#58), while the budget lasts: broken rigid springs rejoin at
    /// the length they hang at, and every bent one moves `MEND_STEP` back towards its built
    /// length, so the dents pull out over several seconds. Spends what it fixes.
    pub fn mend(&mut self) {
        if self.budget == 0 {
            return;
        }
        self.mending = true;
        let before = self.hurt();
        let atoms = &self.atoms;
        for s in self
            .springs
            .iter_mut()
            .filter(|s| s.kind & 0x7f == 1 && s.built >= 0)
        {
            let d = [0, 1, 2].map(|k| atoms[s.a].pos[k].wrapping_sub(atoms[s.b].pos[k]) as f64);
            let len = ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().round() as i64) >> 10;
            // ponytail: a part flung > 512 units off stays off; instant repair (F) still mends it
            let Ok(len) = i16::try_from(len) else {
                continue;
            };
            if s.kind & 0x80 != 0 {
                s.kind &= 0x7f;
                (s.rest, s.aux) = (len, len as u16);
                s.bent = (len as i32 - s.built as i32)
                    .unsigned_abs()
                    .min(u16::MAX as u32) as u16;
            }
            // towards the built length, but never so far from the present one that the spring
            // breaks (stretched) or yields (squeezed) again: half of `cfg` either way
            let (lo, hi) = (len.saturating_sub(0x800), len.saturating_add(0x400));
            let to = s.rest + (s.built - s.rest).clamp(-MEND_STEP, MEND_STEP);
            s.rest = to.clamp(lo.min(s.rest), hi.max(s.rest));
            s.aux = s.rest as u16;
            // what's left of the bend: `bent` sums every yield, back and forth
            let left = (s.built as i32 - s.rest as i32).unsigned_abs() as u16;
            s.bent = s.bent.saturating_sub(MEND_STEP as u16).min(left);
        }
        let ((was, most), (now, _)) = (before, self.hurt());
        let cost = (was - now).max(0) * REPAIR_FULL as i64 / most;
        self.budget = self.budget.saturating_sub(cost as u32);
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
    /// FNV-1a over every car's atoms (positions, velocities) and the terrain heights: equal on
    /// two machines exactly when their runs are, for desync checks in networked play.
    pub fn state_hash(&self) -> u64 {
        self.cars
            .iter()
            .flat_map(|c| c.atoms.iter())
            .flat_map(|a| a.pos.into_iter().chain(a.vel))
            .chain(self.terrain.height.iter().map(|&h| h as i32))
            .fold(0xcbf29ce484222325u64, |h, v| {
                (h ^ v as u32 as u64).wrapping_mul(0x100000001b3)
            })
    }

    pub fn new(terrain: Terrain) -> Sim {
        Sim {
            terrain,
            cars: Vec::new(),
            particles: Vec::new(),
            cfg: (0x0800, -0x1000),
            dig_everywhere: false,
            soft: vec![true; SIZE * SIZE],
            soft_dirt: vec![false; SIZE * SIZE],
            rut_floor: vec![0; SIZE * SIZE],
            trunks: Vec::new(),
            impacts: 0,
            gradual_repair: false,
        }
    }

    /// Swaps in a new map (hot reload): clears dirt particles and the soft-ground marks.
    pub fn set_terrain(&mut self, terrain: Terrain) {
        self.terrain = terrain;
        self.particles.clear();
        self.soft = vec![true; SIZE * SIZE];
        self.set_soft_dirt(vec![false; SIZE * SIZE]);
    }

    /// Marks soft dirt cells; ruts there stop `MAX_RUT` steps below today's heights.
    pub fn set_soft_dirt(&mut self, cells: Vec<bool>) {
        self.rut_floor = self
            .terrain
            .height
            .iter()
            .map(|h| h.saturating_sub(MAX_RUT))
            .collect();
        self.soft_dirt = cells;
    }

    /// The scenery the cars hit. Kept across `set_terrain`: it's the map's, not the ground's.
    pub fn set_trunks(&mut self, trunks: Vec<Trunk>) {
        self.trunks = trunks;
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

    /// The COL colour each live particle was thrown up from (0x0f: soft dirt), for the skid sound.
    pub fn particle_colours(&self) -> impl Iterator<Item = u8> + '_ {
        self.particles.iter().map(|p| p.colour as u8)
    }

    /// One 120 Hz timer tick (`0x5680`). `keys[i]` drives car `i`.
    pub fn tick(&mut self, keys: &[Keys]) {
        for i in 0..self.cars.len() {
            let car = &mut self.cars[i];
            car.input(keys.get(i).copied().unwrap_or_default());
            car.budget = (car.budget + REPAIR_RECHARGE).min(REPAIR_CAP);
            car.drive();
            self.springs(i);
            self.atoms(i);
            self.hit_trunks(i);
            self.cars[i].mending = false;
        }
        self.collide_cars();
        self.step_particles();
    }

    /// `0x500b` / `0x51bd`: for each pair of cars within 0x200 units of each other, push car B's
    /// atoms that are inside car A out across A's nearest box face (least penetration), and kick
    /// A's nearest atom back by half as much, like the original's response.
    fn collide_cars(&mut self) {
        for ia in 0..self.cars.len() {
            for ib in 0..self.cars.len() {
                let (pa, pb) = (self.cars[ia].atoms[0].pos, self.cars[ib].atoms[0].pos);
                if ia == ib || (0..3).any(|k| (pa[k].wrapping_sub(pb[k]) >> 16).abs() > 0x200) {
                    continue;
                }
                let (centre, axes, ext) = self.cars[ia].bounds();
                let va = self.cars[ia].atoms[0].vel;
                let to_local =
                    |rel: [f64; 3]| axes.map(|ax| rel[0] * ax[0] + rel[1] * ax[1] + rel[2] * ax[2]);
                let mut kicks = vec![];
                for (j, p) in self.cars[ib].atoms.iter().enumerate() {
                    let rel = [0, 1, 2].map(|k| p.pos[k].wrapping_sub(pa[k]) as f64 - centre[k]);
                    let local = to_local(rel);
                    let pen = [0, 1, 2].map(|k| ext[k] - local[k].abs());
                    if pen.iter().any(|&d| d <= 0.0) {
                        continue;
                    }
                    // out through the face it came in by: the axis it was still outside on a tick
                    // ago (relative to A); if none, the shallowest
                    let before =
                        to_local([0, 1, 2].map(|k| rel[k] - p.vel[k].wrapping_sub(va[k]) as f64));
                    let entered = |k: usize| before[k].abs() >= ext[k];
                    let k = (0..3)
                        .filter(|&k| entered(k))
                        .min_by(|&a, &b| pen[a].total_cmp(&pen[b]))
                        .unwrap_or_else(|| {
                            (0..3).min_by(|&a, &b| pen[a].total_cmp(&pen[b])).unwrap()
                        });
                    // out across face k, half the depth per tick
                    let push = axes[k].map(|c| (c * local[k].signum() * pen[k] / 2.0) as i32);
                    let qa = (0..self.cars[ia].atoms.len())
                        .min_by_key(|&i| {
                            let q = self.cars[ia].atoms[i].pos;
                            (0..3)
                                .map(|k| (q[k].wrapping_sub(p.pos[k]) >> 16).abs())
                                .sum::<i32>()
                        })
                        .unwrap();
                    kicks.push((j, qa, push));
                }
                for (j, qa, push) in kicks {
                    for k in 0..3 {
                        let v = &mut self.cars[ib].atoms[j].vel[k];
                        *v = v.wrapping_add(push[k]);
                        let v = &mut self.cars[ia].atoms[qa].vel[k];
                        *v = v.wrapping_sub(push[k] >> 1);
                    }
                }
            }
        }
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
                s.built = s.rest;
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
                if car.mending {
                    // firm, but pushing no harder than a spring that yields: dozens of crushed
                    // springs each pushing their full error would scramble the car and launch it
                    (len, e.clamp(-MEND_PUSH, MEND_PUSH))
                } else if (e as i16) > cfg.0 {
                    let y = e >> 4; // 0x4fe3: yield
                    s.bent = s
                        .bent
                        .saturating_add(y.unsigned_abs().min(u16::MAX as u32) as u16);
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
        if vn <= -0x6_0000 {
            self.impacts += 1;
        }
        let at = &mut self.cars[ci].atoms[i];
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

    /// Trunks (not in the original). A thin trunk would slip between the car's mass points, so
    /// it hits the car's frame: each unbroken spring is a segment, and where one passes within
    /// reach of a trunk, its two atoms are pushed out along the horizontal normal (split by how
    /// near each is) and lose their inward velocity, bouncing back a quarter like `contact`, and
    /// half their slide along the bark, so a car catches on a trunk rather than skating round it.
    /// The normal points to the side the segment was on a tick ago: a fast frame moves about a
    /// thin trunk's width per tick, so it may already be on or past the axis, and is pushed back,
    /// not on through. The sudden stop bends the springs: the same damage as hitting the ground.
    fn hit_trunks(&mut self, ci: usize) {
        // ponytail: every trunk per car per tick (~2000 on a forest map, most rejected on the
        // first line); a per-cell grid if maps get denser
        for ti in 0..self.trunks.len() {
            let t = self.trunks[ti];
            let c = [(t.x as i32) << 16, (t.y as i32) << 16];
            let p0 = self.cars[ci].atoms[0].pos;
            // near enough to touch: the car's far corner plus the widest rock
            if (0..2).any(|k| (p0[k].wrapping_sub(c[k]) >> 16).abs() > 0x300) {
                continue;
            }
            let top = height(&self.terrain, t.x, t.y).0 as i64 + t.h as i64;
            for si in 0..self.cars[ci].springs.len() {
                let s = self.cars[ci].springs[si];
                if s.kind & 0x80 != 0 {
                    continue;
                }
                let car = &self.cars[ci];
                let (a, b) = (car.atoms[s.a], car.atoms[s.b]);
                // 16.16 relative to the trunk's axis, now and a tick ago (atoms() just added vel)
                let rel = |p: [i32; 3], v: [i32; 3], back: i32| {
                    [0, 1].map(|k| p[k].wrapping_sub(v[k] * back).wrapping_sub(c[k]) as i64)
                };
                // the segment's closest point to the axis, f (0–0x10000) of the way from a to b
                let closest = |pa: [i64; 2], pb: [i64; 2]| {
                    let ab = [pb[0] - pa[0], pb[1] - pa[1]];
                    let l2 = ab[0] as i128 * ab[0] as i128 + ab[1] as i128 * ab[1] as i128;
                    let along = -(pa[0] as i128 * ab[0] as i128 + pa[1] as i128 * ab[1] as i128);
                    let f = if l2 == 0 {
                        0
                    } else {
                        ((along << 16) / l2).clamp(0, 0x10000) as i64
                    };
                    ([0, 1].map(|k| pa[k] + (((pb[k] - pa[k]) * f) >> 16)), f)
                };
                let (q, f) = closest(rel(a.pos, a.vel, 0), rel(b.pos, b.vel, 0));
                let (was, _) = closest(rel(a.pos, a.vel, 1), rel(b.pos, b.vel, 1));
                let lerp = |u: i64, v: i64| u + (((v - u) * f) >> 16);
                let radius = lerp(a.radius as i64, b.radius as i64);
                let reach = (t.r as i64 + radius) << 16;
                let z = lerp(a.pos[2] as i64, b.pos[2] as i64) >> 16;
                if q.iter().any(|v| v.abs() >= reach + 0x40_0000) || z - radius > top {
                    continue;
                }
                // out the side it came from: a fast frame moves about a trunk's width a tick, so
                // by now it may be on the axis or past it (dead centre then and now: along +x)
                let len = |v: [i64; 2]| (v[0] * v[0] + v[1] * v[1]).isqrt();
                let (from, dist) = match (len(was), len(q)) {
                    (0, 0) => ([1, 0], 1),
                    (0, d) => (q, d),
                    (d, _) => (was, d),
                };
                let n = from.map(|v| v * 0x10000 / dist); // unit normal, 16.16
                // how far out along it (negative: past the axis), and how far aside
                let out = (q[0] * n[0] + q[1] * n[1]) >> 16;
                let aside2 = (q[0] * q[0] + q[1] * q[1] - out * out).max(0);
                if out >= reach || aside2 >= reach * reach {
                    continue;
                }
                let pen = reach - out;
                for (i, w) in [(s.a, 0x10000 - f), (s.b, f)] {
                    let at = &mut self.cars[ci].atoms[i];
                    let vn = (at.vel[0] as i64 * n[0] + at.vel[1] as i64 * n[1]) >> 16;
                    if (vn * w) >> 16 <= -0x6_0000 {
                        self.impacts += 1;
                    }
                    let e = ((vn.min(0) + (vn.min(0) >> 2)) * w) >> 16;
                    let push = (pen * w) >> 16;
                    for k in 0..2 {
                        // the bark grips: half the slide along it goes too, so a car catches
                        // on a trunk instead of skating round it
                        let slide = at.vel[k] as i64 - ((vn * n[k]) >> 16);
                        let grip = ((slide * w) >> 16) >> 1;
                        at.pos[k] = at.pos[k].wrapping_add(((n[k] * push) >> 16) as i32);
                        at.vel[k] = at.vel[k]
                            .wrapping_sub(((n[k] * e) >> 16) as i32)
                            .wrapping_sub(grip as i32);
                    }
                }
            }
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
        let colour = if (self.dig_everywhere && self.soft[cell]) || self.soft_dirt[cell] {
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
                    let dent = cell(x.wrapping_add(0x80), y.wrapping_add(0x80));
                    let h = &mut self.terrain.height[dent];
                    if !self.soft_dirt[dent] || *h > self.rut_floor[dent] {
                        *h = h.saturating_sub(1); // the dent; the asm's `dec byte` wraps 0 → 255 (a spike), never hit in the shipped game
                    }
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
    fn sine_table_matches_sin() {
        for (i, &v) in SIN_QUARTER.iter().enumerate() {
            let want = (i as f64 * std::f64::consts::FRAC_PI_2 / 1024.0).sin() * 32768.0;
            assert!(
                (v as f64 - want.min(32767.0)).abs() <= 1.0,
                "entry {i}: {v} vs {want}"
            );
        }
        assert_eq!(heading_of(0, -5), 0);
        assert_eq!(heading_of(5, 0), 0x4000, "right of map-up is clockwise");
        assert_eq!(heading_of(0, 5), 0x8000);
    }

    /// A scripted 10 s drive, pinned to its exact end state. Same build, same numbers on every
    /// platform: the release workflow runs this on Windows, macOS and Linux, so float drift
    /// between them (replays depend on its absence) fails here.
    #[test]
    fn scripted_drive_is_bit_exact() {
        crate::need_assets!();
        let mut s = sim();
        for t in 0..TICK_HZ * 10 {
            s.tick(&[Keys {
                throttle: t % 300 < 220,
                left: t % 170 < 40,
                right: t % 230 > 180,
                brake: t % 500 > 470,
                ..Keys::default()
            }]);
        }
        let hash = s.state_hash(); // one car: as this test always hashed it
        assert_eq!(hash, GOLDEN, "{hash:#018x}");
    }
    const GOLDEN: u64 = 0x4711da3abcdc297f;

    #[test]
    fn trig_matches_original_conventions() {
        assert_eq!(sin15(0), 0);
        assert_eq!(sin15(0x4000), 32767);
        assert_eq!(sin15(0xc000), -32767);
        assert_eq!(cos15(0), 32767);
        assert_eq!(atan2(0, 5), 0);
        assert_eq!(atan2(5, 0), 0x4000);
        assert_eq!(atan2(5, 5), 0x2000);
        // within a unit of the true angle, all round the circle
        for (y, x) in (-60..=60).flat_map(|y| (-60..=60).map(move |x| (y * 517 + 3, x * 499 - 7))) {
            let want = (y as f64).atan2(x as f64) / std::f64::consts::TAU * 65536.0;
            let err = (angle_of(y, x) as f64 - want).rem_euclid(65536.0);
            assert!(
                err.min(65536.0 - err) <= 0.75,
                "atan2({y}, {x}): {} vs {want}",
                angle_of(y, x)
            );
        }
    }

    #[test]
    fn car_settles_and_drives() {
        crate::need_assets!();
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
        crate::need_assets!();
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

    /// Same inputs, same run: input replays (#6) and networked play rely on it. The tick is all
    /// integers but exact IEEE sqrt / division (no libm): the same on every platform.
    #[test]
    fn same_inputs_same_run() {
        crate::need_assets!();
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
        crate::need_assets!();
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

    fn flat() -> Terrain {
        Terrain {
            height: vec![40; SIZE * SIZE],
            colour: vec![0x60; SIZE * SIZE],
            palette: [[0; 3]; 256],
        }
    }

    #[test]
    fn soft_dirt_ruts_stop_at_the_floor() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        for soft in [false, true] {
            let mut s = Sim::new(flat());
            s.set_soft_dirt(vec![soft; SIZE * SIZE]);
            s.cars.push(SimCar::spawn(&car, &s.terrain, 0x8000, 0x8000));
            let gas = Keys {
                throttle: true,
                left: true,
                ..Keys::default()
            };
            for _ in 0..TICK_HZ * 40 {
                s.tick(&[gas]); // circling over the same ground again and again
            }
            let deepest = s.terrain.height.iter().map(|&h| 40 - h).max().unwrap();
            if soft {
                assert!(deepest > 1, "ruts deepen with every pass, got {deepest}");
                assert!(deepest <= MAX_RUT, "ruts stop at the floor, got {deepest}");
            } else {
                assert_eq!(deepest, 0, "hard ground doesn't dig");
            }
        }
    }

    #[test]
    fn crashes_dent_r_keeps_them_repair_clears() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        let mut t = flat();
        for x in 0..SIZE {
            t.height[100 * SIZE + x] = 200; // a wall across the road
        }
        let mut s = Sim::new(t);
        s.cars.push(SimCar::spawn(&car, &s.terrain, 0x8000, 0x8000)); // row 128, facing the wall
        assert_eq!(s.cars[0].damage(), 0.0);
        let gas = Keys {
            throttle: true,
            ..Keys::default()
        };
        for _ in 0..TICK_HZ * 12 {
            s.tick(&[gas]);
        }
        let hit = s.cars[0].damage();
        assert!(hit > 0.0, "crashing into the wall dents the car");
        let kept = s.cars[0].respawned(&car, &s.terrain, 0x8000, 0x9000, 0);
        assert_eq!(kept.damage(), hit, "R keeps the dents");
        assert_eq!(
            SimCar::spawn(&car, &s.terrain, 0x8000, 0x9000).damage(),
            0.0
        );

        // gradual repair (#58): nothing without a budget; with one, held long enough, the dents
        // pull out even with the car still driven into the wall, spending what it fixes
        let mut broke = s.cars[0].clone();
        broke.budget = 0;
        broke.mend();
        assert_eq!(broke.damage(), hit, "an empty budget mends nothing");
        assert_eq!(
            s.cars[0].budget(),
            REPAIR_CAP,
            "crashing takes nothing from it"
        );
        s.cars[0].budget = REPAIR_CAP / 2;
        let ticks = TICK_HZ * 12;
        for _ in 0..ticks {
            s.cars[0].mend();
            s.tick(&[gas]);
        }
        let mended = s.cars[0].damage();
        assert!(mended < hit / 10.0, "mended {hit} → {mended}");
        let spent = REPAIR_CAP / 2 + REPAIR_RECHARGE * ticks - s.cars[0].budget();
        let fixed = (hit - mended) * REPAIR_FULL as f32;
        assert!(
            spent as f32 <= fixed && spent as f32 > fixed * 0.9,
            "spent {spent} for {fixed}"
        );
        for _ in 0..TICK_HZ * 91 {
            s.tick(&[Keys::default()]);
        }
        assert_eq!(s.cars[0].budget(), REPAIR_CAP, "refills in ~90 s");
    }

    /// A car driven at a tree stops at it, dented; one driven past runs as if it weren't there.
    #[test]
    fn trees_stop_cars() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        let drive = |trunks: Vec<Trunk>| {
            let mut s = Sim::new(flat());
            s.set_trunks(trunks);
            s.cars.push(SimCar::spawn(&car, &s.terrain, 0x8000, 0x8000)); // facing −y
            let gas = Keys {
                throttle: true,
                ..Keys::default()
            };
            for _ in 0..TICK_HZ * 10 {
                s.tick(&[gas]);
            }
            (s.cars[0].atom_positions(), s.cars[0].damage())
        };
        let tree = |x| Trunk {
            x,
            y: 0x7600,
            r: 20,
            h: 600,
        };
        let open = drive(vec![]);
        assert!(
            open.0.iter().all(|p| p[1] < 0x7400 as f32),
            "unhindered, the car gets well past the tree's row"
        );
        let (hit, damage) = drive(vec![tree(0x7fb0)]); // where it crosses the tree's row
        let y = hit.iter().map(|p| p[1]).sum::<f32>() / hit.len() as f32;
        assert!(y > 0x7600 as f32, "stopped at the tree: centre y {y}");
        // (a moderate bump: the dents come at speed, see fast_cars_dont_tunnel_through_trees)
        let _ = damage;
        assert_eq!(
            drive(vec![tree(0x8400)]),
            open,
            "four cells aside: untouched"
        );
    }

    /// At full speed a frame moves about a thin trunk's width a tick, and dead centre the push
    /// had no side: both slipped through before. A long run-up, then a 15-unit trunk right where
    /// the car would cross its row (it drifts on the way), dead centre and a little aside.
    #[test]
    fn fast_cars_dont_tunnel_through_trees() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        const ROW: u16 = 0x2000;
        // the car's speed just before the row, and its slowest within 3 cells after reaching it
        let run = |trunks: Vec<Trunk>| {
            let mut s = Sim::new(flat());
            s.set_trunks(trunks);
            s.cars.push(SimCar::spawn(&car, &s.terrain, 0x4000, 0x7f00)); // 95 cells of run-up
            let gas = Keys {
                throttle: true,
                ..Keys::default()
            };
            // from 3 cells before the row: the speed then, the slowest for the next second
            let (mut cross, mut before_row, mut slowest) = (None, 0.0f32, f32::MAX);
            let (mut last, mut after) = (s.cars[0].centre(), None);
            for tick in 0..TICK_HZ * 20 {
                s.tick(&[gas]);
                let c = s.cars[0].centre();
                let v = (c[0] - last[0]).hypot(c[1] - last[1]);
                if c[1] > ROW as f32 {
                    cross = Some(c[0]); // where it is when it reaches the row (unhindered)
                }
                if after.is_none() && c[1] - (ROW as f32) < 768.0 {
                    (after, before_row) = (Some(tick), v);
                }
                if after.is_some_and(|t| tick - t < TICK_HZ) {
                    slowest = slowest.min(v);
                }
                last = c;
            }
            (cross, before_row, slowest, s.cars[0].damage())
        };
        let (cross, open_speed, open_slowest, _) = run(vec![]);
        let x = cross.unwrap() as i32;
        assert!(open_speed > 10.0, "it got going: {open_speed} units a tick");
        assert!(
            open_slowest > 10.0,
            "unhindered it doesn't slow: {open_slowest}"
        );
        for dx in [0, 25, -40] {
            let (_, speed, slowest, damage) = run(vec![Trunk {
                x: (x + dx) as u16,
                y: ROW,
                r: 15,
                h: 600,
            }]);
            // near the middle it stops dead; further out the frame glances off, still hit
            let most = if dx.abs() <= 25 { 1.0 / 3.0 } else { 2.0 / 3.0 };
            assert!(
                slowest < speed * most,
                "{dx} aside: the trunk stops it: {speed} → {slowest}"
            );
            assert!(damage > 0.0, "{dx} aside: and dents it");
        }
    }

    #[test]
    fn cars_collide_instead_of_passing_through() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        let mut s = Sim::new(flat());
        // A drives north into B, parked 8 cells ahead facing the same way
        s.cars.push(SimCar::spawn(&car, &s.terrain, 0x8000, 0x8c00));
        s.cars.push(SimCar::spawn(&car, &s.terrain, 0x8000, 0x8400));
        let b0 = s.cars[1].centre();
        let gas = Keys {
            throttle: true,
            ..Keys::default()
        };
        for _ in 0..TICK_HZ * 6 {
            s.tick(&[gas, Keys::default()]);
        }
        let (a, b) = (s.cars[0].centre(), s.cars[1].centre());
        assert!(a[1] > b[1], "A stays behind B: a {a:?} b {b:?}");
        assert!(
            b0[1] - b[1] > 256.0,
            "B got shunted north: {} → {}",
            b0[1],
            b[1]
        );
        for c in &s.cars {
            assert!(c.centre()[2].abs() < 4000.0, "nothing flew off");
        }

        // two cars flat out at each other: they set off veering ~10° (the existing handling), so
        // this is a scrape more than a head-on; it must stay stable, nothing launched
        let mut s = Sim::new(flat());
        s.cars
            .push(SimCar::spawn_facing(&car, &s.terrain, 0x8000, 0x8300, 0));
        s.cars.push(SimCar::spawn_facing(
            &car, &s.terrain, 0x8000, 0x7d00, 0x8000,
        ));
        for _ in 0..TICK_HZ * 6 {
            s.tick(&[gas, gas]);
        }
        for c in &s.cars {
            let [x, y, z] = c.centre();
            assert!(
                (0x6000 as f32..0xa000 as f32).contains(&x)
                    && (0x6000 as f32..0xa000 as f32).contains(&y),
                "stayed near: {x} {y}"
            );
            assert!(z.abs() < 4000.0, "nothing flew off: z {z}");
        }
    }

    #[test]
    fn upside_down_only_on_the_roof() {
        crate::need_assets!();
        let flat = || Terrain {
            height: vec![40; SIZE * SIZE],
            colour: vec![0x60; SIZE * SIZE],
            palette: [[0; 3]; 256],
        };
        for n in 1..=5 {
            let mut car =
                crate::car::parse(&crate::assets::read(&format!("CAR{n}.DAT")).unwrap()).unwrap();
            let s = Sim::new(flat());
            for h in [0u16, 0x2800, 0x6600] {
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
        crate::need_assets!();
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
