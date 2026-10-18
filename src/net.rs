//! Networked 2-player races (#33): deterministic lockstep over plain UDP, no services.
//!
//! The sim computes bit-identical ticks everywhere (integer maths, see `sim::angle_of`), so the
//! two machines only swap inputs: one byte per car per tick (`run::input`). Tick `t` runs once
//! both bytes are in; each side's own input is scheduled `DELAY` ticks ahead to hide the trip.
//! The host is car 0, the one who joins car 1, on both machines.
//!
//! Getting together: the host listens on `PORT`; `Discovery` finds hosts on the LAN by
//! broadcast; a `Joiner` sends Join to an address until the host answers with Start (map, cars,
//! spawns), then both run a `Link`. Every packet carries the inputs the other side hasn't
//! acknowledged yet, so lost packets just get resent by the next one. Every `HASH_EVERY` ticks
//! each side sends `Sim::state_hash`; a mismatch is a desync.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const PORT: u16 = 47800;
/// Ticks between pressing a key and its tick: 4 = 33 ms, enough for a LAN or a nearby host.
pub const DELAY: usize = 4;
const HASH_EVERY: u64 = 120;
const TIMEOUT: Duration = Duration::from_secs(5);
const RESEND: Duration = Duration::from_millis(200);
const MAGIC: &[u8; 4] = b"RFN1";
const VERSION: u8 = 1;

/// What both machines start from; the host decides it.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    /// Map folder, and `Level.hash` of its files: the joiner must have the same ones.
    pub map: String,
    pub map_hash: u64,
    /// All ground diggable (`--soft`).
    pub soft: bool,
    /// Car model 0–4 and skin file ("" = own) of the host (0) and the joiner (1).
    pub cars: [u8; 2],
    pub skins: [String; 2],
    /// Spawn x, y and heading per car.
    pub spawns: [[u16; 3]; 2],
}

#[derive(Debug, PartialEq)]
enum Msg {
    Discover,
    HostInfo {
        map: String,
    },
    Join {
        version: u8,
        car: u8,
        skin: String,
    },
    Start(Start),
    Refused(String),
    Inputs {
        have: u32,
        first: u32,
        bytes: Vec<u8>,
        hash: Option<(u32, u64)>,
    },
    Bye,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = &s.as_bytes()[..s.len().min(255)];
    out.push(b.len() as u8);
    out.extend(b);
}

impl Msg {
    fn encode(&self) -> Vec<u8> {
        let mut o = MAGIC.to_vec();
        match self {
            Msg::Discover => o.push(0),
            Msg::HostInfo { map } => {
                o.push(1);
                put_str(&mut o, map);
            }
            Msg::Join { version, car, skin } => {
                o.extend([2, *version, *car]);
                put_str(&mut o, skin);
            }
            Msg::Start(s) => {
                o.extend([3, VERSION]);
                put_str(&mut o, &s.map);
                o.extend(s.map_hash.to_le_bytes());
                o.push(s.soft as u8);
                for i in 0..2 {
                    o.push(s.cars[i]);
                    put_str(&mut o, &s.skins[i]);
                    s.spawns[i].iter().for_each(|v| o.extend(v.to_le_bytes()));
                }
            }
            Msg::Refused(why) => {
                o.push(4);
                put_str(&mut o, why);
            }
            Msg::Inputs {
                have,
                first,
                bytes,
                hash,
            } => {
                o.push(5);
                o.extend(have.to_le_bytes());
                o.extend(first.to_le_bytes());
                o.extend((bytes.len() as u16).to_le_bytes());
                o.extend(bytes);
                let (t, h) = hash.unwrap_or((u32::MAX, 0));
                o.extend(t.to_le_bytes());
                o.extend(h.to_le_bytes());
            }
            Msg::Bye => o.push(6),
        }
        o
    }

    /// None for anything that isn't a well-formed Reformers packet.
    fn decode(d: &[u8]) -> Option<Msg> {
        let mut r = Reader(d.strip_prefix(MAGIC)?);
        let msg = match r.u8()? {
            0 => Msg::Discover,
            1 => Msg::HostInfo { map: r.str()? },
            2 => Msg::Join {
                version: r.u8()?,
                car: r.u8()?,
                skin: r.str()?,
            },
            3 => {
                if r.u8()? != VERSION {
                    return Some(Msg::Refused("the host runs a different version".into()));
                }
                let (map, map_hash, soft) = (r.str()?, r.u64()?, r.u8()? != 0);
                let mut cars = [0; 2];
                let mut skins = [String::new(), String::new()];
                let mut spawns = [[0; 3]; 2];
                for i in 0..2 {
                    cars[i] = r.u8()?;
                    skins[i] = r.str()?;
                    spawns[i] = [r.u16()?, r.u16()?, r.u16()?];
                }
                Msg::Start(Start {
                    map,
                    map_hash,
                    soft,
                    cars,
                    skins,
                    spawns,
                })
            }
            4 => Msg::Refused(r.str()?),
            5 => {
                let (have, first) = (r.u32()?, r.u32()?);
                let n = r.u16()? as usize;
                let bytes = r.take(n)?.to_vec();
                let (t, h) = (r.u32()?, r.u64()?);
                Msg::Inputs {
                    have,
                    first,
                    bytes,
                    hash: (t != u32::MAX).then_some((t, h)),
                }
            }
            6 => Msg::Bye,
            _ => return None,
        };
        Some(msg)
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (a, b) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u8()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// This machine's address on the LAN (the route out; no packet is sent), for the host to show.
pub fn local_ip() -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 168, 0, 1), PORT)).ok()?;
    Some(s.local_addr().ok()?.ip())
}

fn socket(port: u16) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    s.set_nonblocking(true)?;
    Ok(s)
}

/// Everything waiting on the socket. Errors other than "nothing there" are skipped (Windows
/// reports a closed peer port as an error on the next receive).
fn received(sock: &UdpSocket) -> Vec<(SocketAddr, Msg)> {
    let mut buf = [0u8; 2048];
    let mut out = vec![];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => out.extend(Msg::decode(&buf[..n]).map(|m| (from, m))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return out,
            Err(_) => {}
        }
    }
}

fn send(sock: &UdpSocket, to: SocketAddr, msg: &Msg) {
    let _ = sock.send_to(&msg.encode(), to); // a lost packet is resent by the next
}

/// Waiting for a player: answers LAN discovery, and returns who asks to join.
pub struct Host {
    sock: UdpSocket,
}

impl Host {
    pub fn bind(port: u16) -> std::io::Result<Host> {
        Ok(Host {
            sock: socket(port)?,
        })
    }

    pub fn port(&self) -> u16 {
        self.sock.local_addr().map_or(0, |a| a.port())
    }

    /// Answers discovery with `map`; returns the first player asking to join: their address,
    /// car and skin. A different version is refused here.
    pub fn poll(&mut self, map: &str) -> Option<(SocketAddr, u8, String)> {
        for (from, msg) in received(&self.sock) {
            match msg {
                Msg::Discover => send(&self.sock, from, &Msg::HostInfo { map: map.into() }),
                Msg::Join { version, .. } if version != VERSION => send(
                    &self.sock,
                    from,
                    &Msg::Refused("the host runs a different version".into()),
                ),
                Msg::Join { car, skin, .. } => return Some((from, car, skin)),
                _ => {}
            }
        }
        None
    }

    /// The race starts: host is car 0. Start goes out with every packet until the joiner's
    /// first inputs show it arrived.
    pub fn start(self, peer: SocketAddr, start: Start) -> Link {
        Link::new(self.sock, peer, 0, Some(start))
    }
}

/// Asking a host to let us in.
pub struct Joiner {
    sock: UdpSocket,
    host: SocketAddr,
    car: u8,
    skin: String,
    sent: Option<Instant>,
    asked: Instant,
}

pub enum JoinState {
    Waiting,
    Started(Start),
    Failed(String),
}

impl Joiner {
    pub fn new(host: SocketAddr, car: u8, skin: &str) -> std::io::Result<Joiner> {
        Ok(Joiner {
            sock: socket(0)?,
            host,
            car,
            skin: skin.into(),
            sent: None,
            asked: Instant::now(),
        })
    }

    pub fn poll(&mut self) -> JoinState {
        if self.sent.is_none_or(|t| t.elapsed() > RESEND) {
            let join = Msg::Join {
                version: VERSION,
                car: self.car,
                skin: self.skin.clone(),
            };
            send(&self.sock, self.host, &join);
            self.sent = Some(Instant::now());
        }
        for (from, msg) in received(&self.sock) {
            match msg {
                Msg::Start(s) if from == self.host => return JoinState::Started(s),
                Msg::Refused(why) => return JoinState::Failed(why),
                _ => {}
            }
        }
        if self.asked.elapsed() > TIMEOUT {
            return JoinState::Failed(format!("no answer from {}", self.host));
        }
        JoinState::Waiting
    }

    /// The race starts: we're car 1.
    pub fn start(self) -> Link {
        Link::new(self.sock, self.host, 1, None)
    }
}

/// Finding hosts on the LAN: broadcast Discover, collect who answers.
pub struct Discovery {
    sock: Option<UdpSocket>,
    port: u16,
    sent: Option<Instant>,
    /// Hosts that answered: address and the map they're on.
    pub found: Vec<(SocketAddr, String)>,
}

impl Discovery {
    pub fn new(port: u16) -> Discovery {
        let sock = socket(0).ok().filter(|s| s.set_broadcast(true).is_ok());
        Discovery {
            sock,
            port,
            sent: None,
            found: vec![],
        }
    }

    pub fn poll(&mut self) {
        let Some(sock) = &self.sock else { return };
        if self
            .sent
            .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
        {
            for ip in [Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST] {
                send(sock, (ip, self.port).into(), &Msg::Discover);
            }
            self.sent = Some(Instant::now());
        }
        for (from, msg) in received(sock) {
            if let Msg::HostInfo { map } = msg {
                match self.found.iter_mut().find(|(a, _)| *a == from) {
                    Some(f) => f.1 = map,
                    None => self.found.push((from, map)),
                }
            }
        }
    }
}

/// A running race: both players' inputs, tick by tick.
pub struct Link {
    sock: UdpSocket,
    peer: SocketAddr,
    /// Our car: 0 host, 1 joiner.
    pub me: usize,
    /// Inputs by tick, ours and theirs; the first `DELAY` ticks run on no keys.
    local: Vec<u8>,
    remote: Vec<u8>,
    /// How many of our inputs the peer has.
    peer_has: usize,
    /// Host: the Start to repeat until the joiner's first packet.
    start: Option<Start>,
    /// Our recent state hashes (tick, hash), and the latest one we sent.
    hashes: Vec<(u64, u64)>,
    sent: Option<Instant>,
    heard: Instant,
    /// Set when the sims disagree (the tick), or the peer left / went silent.
    pub desync: Option<u64>,
    pub lost: bool,
}

impl Link {
    fn new(sock: UdpSocket, peer: SocketAddr, me: usize, start: Option<Start>) -> Link {
        Link {
            sock,
            peer,
            me,
            local: vec![0; DELAY],
            remote: vec![0; DELAY],
            peer_has: 0,
            start,
            hashes: vec![],
            sent: None,
            heard: Instant::now(),
            desync: None,
            lost: false,
        }
    }

    /// Our input for the next tick that hasn't got one (`DELAY` ticks ahead of the sim).
    pub fn push_local(&mut self, input: u8) {
        self.local.push(input);
    }

    /// How many ticks have our input.
    pub fn local_ticks(&self) -> usize {
        self.local.len()
    }

    /// Both cars' inputs for tick `t` (0-based), in car order, once both are in.
    pub fn inputs(&self, t: usize) -> Option<[u8; 2]> {
        let (l, r) = (*self.local.get(t)?, *self.remote.get(t)?);
        Some(if self.me == 0 { [l, r] } else { [r, l] })
    }

    /// After running tick `t` (1-based count): every `HASH_EVERY` ticks, note the state.
    pub fn ran(&mut self, t: u64, hash: impl FnOnce() -> u64) {
        if t.is_multiple_of(HASH_EVERY) {
            self.hashes.push((t, hash()));
            if self.hashes.len() > 64 {
                self.hashes.remove(0);
            }
        }
    }

    /// Receives what's there and sends what the peer lacks. Call once per frame.
    pub fn poll(&mut self) {
        for (from, msg) in received(&self.sock) {
            if from != self.peer {
                continue;
            }
            self.heard = Instant::now();
            match msg {
                Msg::Inputs {
                    have,
                    first,
                    bytes,
                    hash,
                } => {
                    self.start = None; // the joiner is running: it got Start
                    self.peer_has = self.peer_has.max(have as usize);
                    for (i, b) in bytes.into_iter().enumerate() {
                        if first as usize + i == self.remote.len() {
                            self.remote.push(b);
                        }
                    }
                    if let Some((t, h)) = hash
                        && let Some(&(_, mine)) = self.hashes.iter().find(|x| x.0 == t as u64)
                        && mine != h
                    {
                        self.desync.get_or_insert(t as u64);
                    }
                }
                Msg::Bye => self.lost = true,
                _ => {}
            }
        }
        if self.heard.elapsed() > TIMEOUT {
            self.lost = true;
        }
        if let Some(s) = &self.start
            && self.sent.is_none_or(|t| t.elapsed() > RESEND)
        {
            send(&self.sock, self.peer, &Msg::Start(s.clone()));
        }
        let from = self.peer_has.min(self.local.len());
        let msg = Msg::Inputs {
            have: self.remote.len() as u32,
            first: from as u32,
            bytes: self.local[from..(from + 1024).min(self.local.len())].to_vec(),
            hash: self.hashes.last().map(|&(t, h)| (t as u32, h)),
        };
        send(&self.sock, self.peer, &msg);
        self.sent = Some(Instant::now());
    }

    /// Tells the peer we're leaving.
    pub fn bye(&self) {
        for _ in 0..3 {
            send(&self.sock, self.peer, &Msg::Bye);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run;
    use crate::sim::{Keys, Sim, SimCar};
    use crate::terrain::Terrain;
    use crate::track::Race;

    #[test]
    fn messages_round_trip() {
        let start = Start {
            map: "rcoaster".into(),
            map_hash: 0x0123_4567_89ab_cdef,
            soft: true,
            cars: [2, 4],
            skins: ["HUFO.PCX".into(), String::new()],
            spawns: [[0x8000, 0x7a00, 0x4000], [0x8180, 0x7a00, 0x4000]],
        };
        for m in [
            Msg::Discover,
            Msg::HostInfo {
                map: "terep2".into(),
            },
            Msg::Join {
                version: VERSION,
                car: 3,
                skin: "GHOST.PCX".into(),
            },
            Msg::Start(start),
            Msg::Refused("full".into()),
            Msg::Inputs {
                have: 9,
                first: 4,
                bytes: vec![1, 5, 33],
                hash: Some((120, 77)),
            },
            Msg::Inputs {
                have: 0,
                first: 0,
                bytes: vec![],
                hash: None,
            },
            Msg::Bye,
        ] {
            assert_eq!(Msg::decode(&m.encode()), Some(m));
        }
        assert_eq!(Msg::decode(b"RFN1\x05\x01"), None, "truncated");
        assert_eq!(Msg::decode(b"HTTP/1.1"), None, "not ours");
    }

    /// Host and joiner over localhost UDP, each with its own sim, driving scripted keys: they
    /// handshake, run 6 s in lockstep, end bit-identical, and match one sim fed both inputs.
    #[test]
    fn two_machines_stay_in_lockstep() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let new_sim = || {
            let mut s = Sim::new(
                Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap(),
            );
            s.cars
                .push(SimCar::spawn_facing(&car, &s.terrain, 0x8000, 0x7a00, 0));
            s.cars
                .push(SimCar::spawn_facing(&car, &s.terrain, 0x8180, 0x7a00, 0));
            s
        };
        // what each player presses on tick t
        let keys = |me: usize, t: usize| {
            run::input(
                Keys {
                    throttle: (t + me * 37) % 300 < 220,
                    left: me == 0 && t % 200 < 30,
                    right: me == 1 && t % 260 < 40,
                    ..Keys::default()
                },
                t == 400 + me,
                false,
            )
        };
        let host = Host::bind(0).unwrap();
        let addr: SocketAddr = (Ipv4Addr::LOCALHOST, host.port()).into();
        let start = Start {
            map: "terep2".into(),
            map_hash: 1,
            soft: false,
            cars: [0, 0],
            skins: [String::new(), String::new()],
            spawns: [[0x8000, 0x7a00, 0], [0x8180, 0x7a00, 0]],
        };
        let (mut host, mut joiner) = (Some(host), Some(Joiner::new(addr, 0, "").unwrap()));
        let (mut host_link, mut joiner_link) = (None, None);
        let t0 = Instant::now();
        while host_link.is_none() || joiner_link.is_none() {
            assert!(t0.elapsed() < Duration::from_secs(3), "handshake");
            if let Some(h) = &mut host
                && let Some((from, car, _)) = h.poll("terep2")
            {
                assert_eq!(car, 0);
                host_link = Some(host.take().unwrap().start(from, start.clone()));
            }
            if let Some(l) = &mut host_link {
                l.poll();
            }
            if let Some(j) = &mut joiner {
                match j.poll() {
                    JoinState::Started(s) => {
                        assert_eq!(s, start);
                        joiner_link = Some(joiner.take().unwrap().start());
                    }
                    JoinState::Failed(e) => panic!("{e}"),
                    JoinState::Waiting => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        lockstep(host_link.unwrap(), joiner_link.unwrap(), new_sim, keys);

        fn lockstep(
            host: Link,
            joiner: Link,
            new_sim: impl Fn() -> Sim,
            keys: impl Fn(usize, usize) -> u8,
        ) {
            let car = crate::car::parse(&crate::assets::read("CAR1.DAT").unwrap()).unwrap();
            let ticks = 6 * crate::sim::TICK_HZ as usize;
            let mut sides = [(host, new_sim(), 0usize), (joiner, new_sim(), 0usize)];
            let t0 = Instant::now();
            while sides.iter().any(|s| s.2 < ticks) {
                assert!(t0.elapsed() < Duration::from_secs(20), "lockstep stalled");
                for (link, sim, t) in sides.iter_mut() {
                    link.poll();
                    while link.local_ticks() < *t + DELAY + 1 {
                        let me = link.me;
                        let n = link.local_ticks() - DELAY;
                        link.push_local(keys(me, n));
                    }
                    while *t < ticks
                        && let Some(inputs) = link.inputs(*t)
                    {
                        let mut races = [Race::new(None), Race::new(None)];
                        run::tick_all(sim, &[&car, &car], None, &mut races, &inputs, *t as u64);
                        *t += 1;
                        link.ran(*t as u64, || sim.state_hash());
                    }
                    assert_eq!(link.desync, None);
                }
            }
            for _ in 0..5 {
                sides.iter_mut().for_each(|s| s.0.poll());
            }
            assert_eq!(sides[0].1.state_hash(), sides[1].1.state_hash());
            // and one sim fed both players' inputs, each DELAY ticks late, agrees
            let mut reference = new_sim();
            for t in 0..ticks {
                let late = |me| if t < DELAY { 0 } else { keys(me, t - DELAY) };
                let mut races = [Race::new(None), Race::new(None)];
                run::tick_all(
                    &mut reference,
                    &[&car, &car],
                    None,
                    &mut races,
                    &[late(0), late(1)],
                    t as u64,
                );
            }
            assert_eq!(reference.state_hash(), sides[0].1.state_hash());
            assert!(sides.iter().all(|s| !s.0.lost));
        }
    }
}
