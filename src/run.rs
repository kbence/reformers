//! One tick of play as the game runs it — inputs (driving keys, R, F) in, sim and time trial
//! out — and input replays: a whole run recorded as one input byte per tick, which re-drives
//! it bit-exactly because the sim is deterministic (see `sim::tests::scripted_drive_is_bit_exact`).

use crate::car::Car;
use crate::sim::{Keys, Sim, SimCar};
use crate::track::{Event, Race, Track};

const THROTTLE: u8 = 1;
const REVERSE: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;
const BRAKE: u8 = 16;
/// R: back on the wheels (on the last checkpoint in a time trial), dents kept.
pub const RESPAWN: u8 = 32;
/// F: a fresh car where this one stands, or with `Sim::gradual_repair` a tick of mending (held).
pub const REPAIR: u8 = 64;
/// T: back on the wheels where it stands, dents kept (#58). With `REPAIR` under gradual repair
/// (F double-tapped): a fresh car where it stands, if the budget is full, which it then spends.
pub const TURN_OVER: u8 = 128;

/// World units the body may be out of shape (`SimCar::misshapen`) with its springs whole before
/// gradual repair snaps it back; a sound car reads under 5, even mid-jump.
const FOLDED: f64 = 12.0;

/// One tick's input byte.
pub fn input(k: Keys, respawn: bool, repair: bool, turn_over: bool) -> u8 {
    [
        (k.throttle, THROTTLE),
        (k.reverse, REVERSE),
        (k.left, LEFT),
        (k.right, RIGHT),
        (k.brake, BRAKE),
        (respawn, RESPAWN),
        (repair, REPAIR),
        (turn_over, TURN_OVER),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0, |b, (_, bit)| b | bit)
}

pub fn keys(input: u8) -> Keys {
    Keys {
        throttle: input & THROTTLE != 0,
        reverse: input & REVERSE != 0,
        left: input & LEFT != 0,
        right: input & RIGHT != 0,
        brake: input & BRAKE != 0,
    }
}

/// What a tick did, for effects outside the sim (sounds, banners, wheel spin reset).
pub struct Tick {
    pub event: Option<Event>,
    pub respawned: bool,
}

/// R: on the last checkpoint facing the next (time trial), else upright where it is.
/// Back on the wheels, dents kept: at the last checkpoint in a time trial (R), or where it
/// stands (`in_place`, turn over).
fn respawn(
    sim: &mut Sim,
    i: usize,
    car: &Car,
    track: Option<&Track>,
    race: &mut Race,
    in_place: bool,
) {
    let [x, y, _] = sim.cars[i].centre();
    let ([x, y], h) = track
        .filter(|_| !in_place)
        .and_then(|t| race.respawn(t))
        .unwrap_or(([x, y], sim.cars[i].heading()));
    sim.cars[i] = sim.cars[i].respawned(car, &sim.terrain, x as u16, y as u16, h);
    if let Some(t) = track {
        race.place(t, [x, y]);
    }
}

/// One 120 Hz tick of car 0 with this input byte; `t` is the tick count for lap times.
pub fn tick(
    sim: &mut Sim,
    car: &Car,
    track: Option<&Track>,
    race: &mut Race,
    input: u8,
    t: u64,
) -> Tick {
    tick_all(sim, &[car], track, std::slice::from_mut(race), &[input], t)
        .pop()
        .unwrap()
}

/// One tick of every car `i` (model `cars[i]`, time trial `races[i]`, input `inputs[i]`), all
/// stepped together so they can collide.
pub fn tick_all(
    sim: &mut Sim,
    cars: &[&Car],
    track: Option<&Track>,
    races: &mut [Race],
    inputs: &[u8],
    t: u64,
) -> Vec<Tick> {
    let mut respawned = vec![false; inputs.len()];
    for (i, &input) in inputs.iter().enumerate() {
        let full = REPAIR | TURN_OVER;
        if input & full == full && sim.gradual_repair {
            // double-tapped repair: a fresh car where it stands, for a full bar
            if sim.cars[i].was_full() {
                let [x, y, _] = sim.cars[i].centre();
                let h = sim.cars[i].heading();
                sim.cars[i] = SimCar::spawn_facing(cars[i], &sim.terrain, x as u16, y as u16, h);
                sim.cars[i].spend_all();
                respawned[i] = true;
            }
        } else if input & REPAIR != 0 && sim.gradual_repair {
            sim.cars[i].mend();
            // springs whole but the body still folded (a big wreck): back in shape where it stands
            if sim.cars[i].whole() && sim.cars[i].misshapen() > FOLDED {
                respawn(sim, i, cars[i], track, &mut races[i], true);
                respawned[i] = true;
            }
        } else if input & REPAIR != 0 {
            let [x, y, _] = sim.cars[i].centre();
            let h = sim.cars[i].heading();
            sim.cars[i] = SimCar::spawn_facing(cars[i], &sim.terrain, x as u16, y as u16, h);
            respawned[i] = true;
        }
        if input & RESPAWN != 0 {
            respawn(sim, i, cars[i], track, &mut races[i], false);
            respawned[i] = true;
        }
        if input & TURN_OVER != 0 && !(sim.gradual_repair && input & REPAIR != 0) {
            respawn(sim, i, cars[i], track, &mut races[i], true);
            respawned[i] = true;
        }
    }
    let keys: Vec<Keys> = inputs.iter().map(|&b| keys(b)).collect();
    sim.tick(&keys);
    (0..inputs.len())
        .map(|i| {
            if let Some(tr) = track
                && tr.apply_zone(&mut sim.cars[i])
            {
                respawn(sim, i, cars[i], track, &mut races[i], false);
                respawned[i] = true;
            }
            let event = track.and_then(|tr| {
                let [x, y, _] = sim.cars[i].centre();
                races[i].update(tr, [x, y], t)
            });
            Tick {
                event,
                respawned: respawned[i],
            }
        })
        .collect()
}

/// A recorded run: where and how it started, then one input byte per tick.
#[derive(Debug, PartialEq)]
pub struct Replay {
    /// Map folder name, and the hash of its MAP/COL/CHECK/OBJECTS files (it only replays on those).
    pub map: String,
    pub key: u64,
    /// Car 1–5.
    pub car: u8,
    /// Started with `--soft` (all ground diggable).
    pub soft: bool,
    /// Gradual repair (`Sim::gradual_repair`), stored in the soft byte's bit 1.
    pub gradual: bool,
    /// Spawn x, y (world units) and heading (`SimCar::spawn_facing`).
    pub start: [u16; 3],
    pub inputs: Vec<u8>,
}

const MAGIC: &[u8; 4] = b"RFR1";

impl Replay {
    /// `RFR1`, map name (u8 length + bytes), key (u64), car, flags (1 soft, 2 gradual repair), start x/y/heading (u16 ×3),
    /// tick count (u32), then (run length u16, input byte) pairs. All little-endian.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(self.map.len() as u8);
        out.extend(self.map.as_bytes());
        out.extend(self.key.to_le_bytes());
        out.extend([self.car, self.soft as u8 | (self.gradual as u8) << 1]);
        self.start.iter().for_each(|v| out.extend(v.to_le_bytes()));
        out.extend((self.inputs.len() as u32).to_le_bytes());
        let mut i = 0;
        while i < self.inputs.len() {
            let b = self.inputs[i];
            let n = self.inputs[i..]
                .iter()
                .take(u16::MAX as usize)
                .take_while(|&&x| x == b)
                .count();
            out.extend((n as u16).to_le_bytes());
            out.push(b);
            i += n;
        }
        out
    }

    pub fn from_bytes(d: &[u8]) -> Result<Replay, String> {
        let bad = || "not a Reformers replay (or a damaged one)".to_string();
        if d.get(..4) != Some(MAGIC) {
            return Err(bad());
        }
        let n = *d.get(4).ok_or_else(bad)? as usize;
        let map =
            String::from_utf8(d.get(5..5 + n).ok_or_else(bad)?.to_vec()).map_err(|_| bad())?;
        let h = d.get(5 + n..5 + n + 20).ok_or_else(bad)?;
        let u16_at = |o: usize| u16::from_le_bytes([h[o], h[o + 1]]);
        let ticks = u32::from_le_bytes(h[16..20].try_into().unwrap()) as usize;
        let mut inputs = Vec::with_capacity(ticks);
        for run in d[5 + n + 20..].chunks(3) {
            let [a, b, byte] = run else { return Err(bad()) };
            inputs.extend(std::iter::repeat_n(
                *byte,
                u16::from_le_bytes([*a, *b]) as usize,
            ));
        }
        if inputs.len() != ticks {
            return Err(bad());
        }
        Ok(Replay {
            map,
            key: u64::from_le_bytes(h[0..8].try_into().unwrap()),
            car: h[8],
            soft: h[9] & 1 != 0,
            gradual: h[9] & 2 != 0,
            start: [u16_at(10), u16_at(12), u16_at(14)],
            inputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{REPAIR_CAP, SimCar, TICK_HZ};
    use crate::terrain::{SIZE, Terrain};

    /// Gradual repair held long enough after a wreck leaves the car as built: springs mended,
    /// broken ones rejoined, and the body back in shape even where the right lengths had
    /// settled folded, without the repair throwing the car about (a crash into a wall, then a
    /// long drop).
    #[test]
    fn gradual_repair_ends_as_built() {
        crate::need_assets!();
        let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
        let mut t = Terrain {
            height: vec![40; SIZE * SIZE],
            colour: vec![0x60; SIZE * SIZE],
            palette: [[0; 3]; 256],
        };
        for x in 0..SIZE {
            t.height[100 * SIZE + x] = 200; // a wall across the road
        }
        let mut sim = Sim::new(t);
        sim.gradual_repair = true;
        sim.cars
            .push(SimCar::spawn(&car, &sim.terrain, 0x8000, 0x8000));
        let mut race = Race::new(None);
        let gas = input(
            Keys {
                throttle: true,
                ..Keys::default()
            },
            false,
            false,
            false,
        );
        for t in 0..TICK_HZ as u64 * 12 {
            tick(&mut sim, &car, None, &mut race, gas, t);
        }
        sim.cars[0] = sim.cars[0].respawned(&car, &sim.terrain, 0x8000, 0x9000, 0x2000);
        sim.cars[0].lift(12000);
        for t in 0..TICK_HZ as u64 * 10 {
            tick(&mut sim, &car, None, &mut race, 0, t);
        }
        assert!(sim.cars[0].broken_springs() > 0, "the drop broke it");
        assert!(sim.cars[0].misshapen() > FOLDED, "and bent it");
        let ground = sim.cars[0].centre()[2];
        let fix = input(Keys::default(), false, true, false);
        let mut top = 0f32;
        for t in 0..TICK_HZ as u64 * 30 {
            tick(&mut sim, &car, None, &mut race, fix, t);
            top = top.max(sim.cars[0].centre()[2]);
        }
        assert!(
            top < ground + 200.0,
            "repairing threw it from {ground} to {top}"
        );
        let c = &sim.cars[0];
        assert!(c.whole(), "damage left: {}", c.damage());
        assert!(
            c.misshapen() < FOLDED,
            "still out of shape: {}",
            c.misshapen()
        );

        // double-tapped (repair + turn over): only with a full bar, which it then spends
        let full = input(Keys::default(), false, true, true);
        sim.cars[0].spend_all();
        assert!(
            !tick(&mut sim, &car, None, &mut race, full, 0).respawned,
            "no full repair on a part-empty bar"
        );
        for t in 0..TICK_HZ as u64 * 92 {
            tick(&mut sim, &car, None, &mut race, 0, t);
        }
        assert_eq!(sim.cars[0].budget(), REPAIR_CAP);
        // dented again, then tapped twice: the first press mends a little, the second still counts
        sim.cars[0].lift(12000);
        for t in 0..TICK_HZ as u64 * 10 {
            tick(&mut sim, &car, None, &mut race, 0, t);
        }
        let fix = input(Keys::default(), false, true, false);
        for t in 0..20 {
            tick(&mut sim, &car, None, &mut race, fix, t);
        }
        for t in 0..10 {
            tick(&mut sim, &car, None, &mut race, 0, t);
        }
        assert!(sim.cars[0].budget() < REPAIR_CAP, "the first press mended");
        assert!(tick(&mut sim, &car, None, &mut race, full, 0).respawned);
        assert!(
            sim.cars[0].budget() < REPAIR_CAP / 100,
            "a full repair takes the bar"
        );
    }

    #[test]
    fn input_bits_round_trip() {
        let k = Keys {
            throttle: true,
            right: true,
            ..Keys::default()
        };
        let b = input(k, true, false, true);
        assert_eq!(b, THROTTLE | RIGHT | RESPAWN | TURN_OVER);
        let back = keys(b);
        assert!(back.throttle && back.right && !back.left && !back.brake && !back.reverse);
    }

    #[test]
    fn replay_bytes_round_trip() {
        let r = Replay {
            map: "terep2".into(),
            key: 0x0123_4567_89ab_cdef,
            car: 3,
            soft: true,
            gradual: true,
            start: [0x7a00, 0x8900, 0x4000],
            inputs: [vec![1u8; 70_000], vec![0, 5, 5, 96]].concat(),
        };
        let b = r.to_bytes();
        assert!(b.len() < 60, "runs compress: {} bytes", b.len());
        assert_eq!(Replay::from_bytes(&b).unwrap(), r);
        assert!(Replay::from_bytes(&b[..b.len() - 1]).is_err());
        assert!(Replay::from_bytes(b"RFR0").is_err());
    }

    #[test]
    fn two_cars_tick_together() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let track = Track::load(&read("maps/terep2/CHECK.PCX")).unwrap();
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let sp = track.spawn.as_ref().unwrap();
        let mut sim = Sim::new(
            Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap(),
        );
        sim.cars.push(SimCar::spawn_facing(
            &car,
            &sim.terrain,
            sp.x,
            sp.y,
            sp.heading,
        ));
        sim.cars.push(SimCar::spawn_facing(
            &car,
            &sim.terrain,
            sp.x + 384,
            sp.y,
            sp.heading,
        ));
        let mut races = [Race::new(None), Race::new(None)];
        let gas = input(
            Keys {
                throttle: true,
                ..Keys::default()
            },
            false,
            false,
            false,
        );
        let mut started = [false; 2];
        for t in 0..TICK_HZ as u64 * 6 {
            // only car 0 drives at first; car 1 sets off two seconds later
            let b1 = if t > TICK_HZ as u64 * 2 { gas } else { 0 };
            for (i, o) in tick_all(
                &mut sim,
                &[&car, &car],
                Some(&track),
                &mut races,
                &[gas, b1],
                t,
            )
            .iter()
            .enumerate()
            {
                started[i] |= o.event == Some(Event::Start);
            }
        }
        assert_eq!(started, [true, true], "each car runs its own time trial");
        assert!(races[0].lap_time(TICK_HZ as u64 * 6) > races[1].lap_time(TICK_HZ as u64 * 6));
    }

    /// Records a scripted run through the time trial (with R and F mixed in), then re-drives
    /// the recording from the same start: the same events at the same ticks, the same car.
    #[test]
    fn replay_re_drives_the_run() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let track = Track::load(&read("maps/terep2/CHECK.PCX")).unwrap();
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let sp = track.spawn.as_ref().unwrap();
        let play = |inputs: &mut dyn FnMut(u64) -> u8| {
            let mut sim = Sim::new(
                Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap(),
            );
            sim.set_soft_dirt(track.soft_dirt.clone());
            sim.cars.push(SimCar::spawn_facing(
                &car,
                &sim.terrain,
                sp.x,
                sp.y,
                sp.heading,
            ));
            let mut race = Race::new(None);
            let mut events = vec![];
            for t in 0..TICK_HZ as u64 * 20 {
                let ev = tick(&mut sim, &car, Some(&track), &mut race, inputs(t), t).event;
                events.extend(ev.map(|e| (t, e)));
            }
            (events, sim.cars[0].atom_positions(), sim.terrain.height)
        };
        let mut recorded = vec![];
        let live = play(&mut |t| {
            let b = input(
                Keys {
                    throttle: t % 400 < 330,
                    left: (900..960).contains(&(t % 1200)),
                    ..Keys::default()
                },
                t == 1500,
                t == 2000,
                t == 2500,
            );
            recorded.push(b);
            b
        });
        assert!(!live.0.is_empty(), "the run reaches the start line");
        let again = play(&mut |t| recorded[t as usize]);
        assert_eq!(again, live);
    }
}
