//! Networked races for up to `MAX_PLAYERS` (#33, #54): deterministic lockstep over plain UDP, no
//! services.
//!
//! The sim computes bit-identical ticks everywhere (integer maths, see `sim::angle_of`), so the
//! machines only swap inputs: one byte per car per tick (`run::input`). Each side's own input is
//! scheduled `DELAY` ticks ahead to hide the trip. The host is car 0; joiners get 1, 2, … in the
//! order they joined.
//!
//! A star: joiners only talk to the host (only the host needs a reachable port). Each joiner
//! sends its own inputs; the host collects everyone's and sends back frames, a byte per car for
//! each tick it has complete, so every machine runs exactly the same ticks. Every packet carries
//! what the other side hasn't acknowledged yet, so a lost packet is resent by the next one. A
//! joiner who leaves or goes silent drives on with no keys (its car rolls to a stop) from where
//! its inputs ran out: the host decides that, and the frames carry it, so all agree. The race
//! ends for everyone if the host goes.
//!
//! Getting together: the host listens on `PORT` and answers LAN `Discovery`; a `Joiner` asks to
//! join until the host says it's in the lobby, then waits for Start (map, every car, its own
//! slot). Every `HASH_EVERY` ticks each side sends `Sim::state_hash`; the host compares each
//! joiner's with its own and the joiners compare the host's; a mismatch is a desync.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const PORT: u16 = 47800;
/// Cars in an online race at most: lockstep waits for the slowest link, so more wants a different
/// netcode (rollback).
pub const MAX_PLAYERS: usize = 4;
/// Ticks between pressing a key and its tick: 4 = 33 ms, enough for a LAN or a nearby host.
pub const DELAY: usize = 4;
const HASH_EVERY: u64 = 120;
const TIMEOUT: Duration = Duration::from_secs(5);
const RESEND: Duration = Duration::from_millis(200);
const MAGIC: &[u8; 4] = b"RFN1";
const VERSION: u8 = 3;
/// Ticks of frames / inputs per packet at most (4 cars × 256 ticks fits the 2 KB buffer).
const BATCH: usize = 256;

/// What every machine starts from; the host decides it.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    /// Map folder, and `Level.hash` of its files: everyone must have the same ones.
    pub map: String,
    pub map_hash: u64,
    /// All ground diggable (`--soft`).
    pub soft: bool,
    /// Gradual repair from a budget (the host's Repair option), sent in the soft byte's bit 1.
    pub gradual: bool,
    /// Per car (0 = host): model 0–4, skin file ("" = own), spawn x, y and heading.
    pub cars: Vec<u8>,
    pub skins: Vec<String>,
    pub spawns: Vec<[u16; 3]>,
    /// Which car is this machine's.
    pub you: u8,
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
    /// Host → joiner waiting to start: you're in, `players` cars so far (host included).
    Lobby {
        players: u8,
    },
    Start(Start),
    Refused(String),
    /// Joiner → host: its inputs from tick `first`; it has `have` frames.
    Inputs {
        have: u32,
        first: u32,
        bytes: Vec<u8>,
        hash: Option<(u32, u64)>,
    },
    /// Host → joiner: complete ticks from `first`, `players` bytes each; the host has `have` of
    /// this joiner's inputs; bit k of `gone` = car k left.
    Frames {
        have: u32,
        first: u32,
        players: u8,
        bytes: Vec<u8>,
        gone: u8,
        hash: Option<(u32, u64)>,
    },
    Bye,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = &s.as_bytes()[..s.len().min(255)];
    out.push(b.len() as u8);
    out.extend(b);
}

fn put_hash(out: &mut Vec<u8>, hash: Option<(u32, u64)>) {
    let (t, h) = hash.unwrap_or((u32::MAX, 0));
    out.extend(t.to_le_bytes());
    out.extend(h.to_le_bytes());
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
                o.extend([
                    s.soft as u8 | (s.gradual as u8) << 1,
                    s.you,
                    s.cars.len() as u8,
                ]);
                for i in 0..s.cars.len() {
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
                put_hash(&mut o, *hash);
            }
            Msg::Bye => o.push(6),
            Msg::Lobby { players } => o.extend([7, *players]),
            Msg::Frames {
                have,
                first,
                players,
                bytes,
                gone,
                hash,
            } => {
                o.push(8);
                o.extend(have.to_le_bytes());
                o.extend(first.to_le_bytes());
                o.extend([*players, *gone]);
                o.extend((bytes.len() as u16).to_le_bytes());
                o.extend(bytes);
                put_hash(&mut o, *hash);
            }
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
                let (map, map_hash) = (r.str()?, r.u64()?);
                let (flags, you, n) = (r.u8()?, r.u8()?, r.u8()? as usize);
                let (soft, gradual) = (flags & 1 != 0, flags & 2 != 0);
                if n > MAX_PLAYERS || you as usize >= n {
                    return None;
                }
                let (mut cars, mut skins, mut spawns) = (vec![], vec![], vec![]);
                for _ in 0..n {
                    cars.push(r.u8()?);
                    skins.push(r.str()?);
                    spawns.push([r.u16()?, r.u16()?, r.u16()?]);
                }
                Msg::Start(Start {
                    map,
                    map_hash,
                    soft,
                    gradual,
                    cars,
                    skins,
                    spawns,
                    you,
                })
            }
            4 => Msg::Refused(r.str()?),
            5 => {
                let (have, first) = (r.u32()?, r.u32()?);
                let n = r.u16()? as usize;
                let bytes = r.take(n)?.to_vec();
                Msg::Inputs {
                    have,
                    first,
                    bytes,
                    hash: r.hash()?,
                }
            }
            6 => Msg::Bye,
            7 => Msg::Lobby { players: r.u8()? },
            8 => {
                let (have, first) = (r.u32()?, r.u32()?);
                let (players, gone) = (r.u8()?, r.u8()?);
                let n = r.u16()? as usize;
                let bytes = r.take(n)?.to_vec();
                if players == 0 || bytes.len() % players as usize != 0 {
                    return None;
                }
                Msg::Frames {
                    have,
                    first,
                    players,
                    bytes,
                    gone,
                    hash: r.hash()?,
                }
            }
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
    /// A (tick, hash) pair, tick `u32::MAX` meaning none yet.
    fn hash(&mut self) -> Option<Option<(u32, u64)>> {
        let (t, h) = (self.u32()?, self.u64()?);
        Some((t != u32::MAX).then_some((t, h)))
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

/// Someone who asked to join: where they are, their car and skin.
#[derive(Clone, Debug, PartialEq)]
pub struct Guest {
    pub addr: SocketAddr,
    pub car: u8,
    pub skin: String,
}

/// The lobby: answers LAN discovery and collects who asks to join, until `start`.
pub struct Host {
    sock: UdpSocket,
    /// In the order they joined: they'll be cars 1, 2, ….
    pub guests: Vec<Guest>,
}

impl Host {
    pub fn bind(port: u16) -> std::io::Result<Host> {
        Ok(Host {
            sock: socket(port)?,
            guests: vec![],
        })
    }

    pub fn port(&self) -> u16 {
        self.sock.local_addr().map_or(0, |a| a.port())
    }

    /// Answers discovery with `map`, and every Join: in the lobby (a new one is added, up to
    /// `MAX_PLAYERS` cars), refused when full or on a different version.
    pub fn poll(&mut self, map: &str) {
        for (from, msg) in received(&self.sock) {
            match msg {
                Msg::Discover => send(&self.sock, from, &Msg::HostInfo { map: map.into() }),
                Msg::Join { version, .. } if version != VERSION => send(
                    &self.sock,
                    from,
                    &Msg::Refused("the host runs a different version".into()),
                ),
                Msg::Join { car, skin, .. } => {
                    let known = self.guests.iter().any(|g| g.addr == from);
                    if !known && self.guests.len() + 1 >= MAX_PLAYERS {
                        send(&self.sock, from, &Msg::Refused("the race is full".into()));
                        continue;
                    }
                    if !known {
                        self.guests.push(Guest {
                            addr: from,
                            car,
                            skin,
                        });
                    }
                    let players = self.guests.len() as u8 + 1;
                    send(&self.sock, from, &Msg::Lobby { players });
                }
                Msg::Bye => self.guests.retain(|g| g.addr != from),
                _ => {}
            }
        }
    }

    /// The race starts: `start` lists every car (the host's first, then the guests' in order;
    /// its `you` is ignored). Each guest gets it with their own slot, repeated until their
    /// first inputs show it arrived.
    pub fn start(self, start: Start) -> Link {
        let peers = self
            .guests
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let mut s = start.clone();
                s.you = i as u8 + 1;
                Peer::new(g.addr, Some(s))
            })
            .collect();
        Link::new(self.sock, peers, 0, start.cars.len())
    }
}

/// Asking a host to let us in, then waiting in its lobby for the start.
pub struct Joiner {
    sock: UdpSocket,
    host: SocketAddr,
    car: u8,
    skin: String,
    sent: Option<Instant>,
    heard: Instant,
    /// Once the host has us: how many cars are in its lobby (it included).
    pub lobby: Option<u8>,
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
            heard: Instant::now(),
            lobby: None,
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
            if from != self.host {
                continue;
            }
            match msg {
                Msg::Lobby { players } => {
                    (self.lobby, self.heard) = (Some(players), Instant::now())
                }
                Msg::Start(s) => return JoinState::Started(s),
                Msg::Refused(why) => return JoinState::Failed(why),
                _ => {}
            }
        }
        if self.heard.elapsed() > TIMEOUT {
            return JoinState::Failed(match self.lobby {
                Some(_) => "the host went quiet".into(),
                None => format!("no answer from {}", self.host),
            });
        }
        JoinState::Waiting
    }

    /// The race starts as `start` says (from `poll`): we're car `start.you`.
    pub fn start(self, start: &Start) -> Link {
        Link::new(
            self.sock,
            vec![Peer::new(self.host, None)],
            start.you as usize,
            start.cars.len(),
        )
    }

    /// Leaves the lobby.
    pub fn bye(&self) {
        send(&self.sock, self.host, &Msg::Bye);
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

/// The other end: for the host, each guest (car i + 1); for a joiner, the host.
struct Peer {
    addr: SocketAddr,
    /// How many of what we send it (host: frames; joiner: our inputs) it has.
    has: usize,
    heard: Instant,
    /// Host: the Start to repeat until this guest's first packet.
    start: Option<Start>,
}

impl Peer {
    fn new(addr: SocketAddr, start: Option<Start>) -> Peer {
        Peer {
            addr,
            has: 0,
            heard: Instant::now(),
            start,
        }
    }
}

/// A running race: every car's inputs, tick by tick.
pub struct Link {
    sock: UdpSocket,
    peers: Vec<Peer>,
    /// Our car: 0 host, 1… joiners.
    pub me: usize,
    /// Cars in the race.
    pub players: usize,
    /// Inputs by car, then tick (host: everyone's as they arrive; joiner: only ours, in
    /// `streams[me]`). The first `DELAY` ticks run on no keys.
    streams: Vec<Vec<u8>>,
    /// Complete ticks, a byte per car: what the sim runs.
    frames: Vec<Vec<u8>>,
    /// Cars whose player left (they get no keys from where their inputs ran out).
    pub gone: Vec<bool>,
    /// Our recent state hashes (tick, hash).
    hashes: Vec<(u64, u64)>,
    sent: Option<Instant>,
    /// Set when the sims disagree (the tick), or the race is over for us: the host left (or,
    /// for the host, every guest did).
    pub desync: Option<u64>,
    pub lost: bool,
}

impl Link {
    fn new(sock: UdpSocket, peers: Vec<Peer>, me: usize, players: usize) -> Link {
        Link {
            sock,
            peers,
            me,
            players,
            streams: vec![vec![0; DELAY]; players],
            frames: (0..DELAY).map(|_| vec![0; players]).collect(),
            gone: vec![false; players],
            hashes: vec![],
            sent: None,
            desync: None,
            lost: false,
        }
    }

    fn host(&self) -> bool {
        self.me == 0
    }

    /// Our input for the next tick that hasn't got one (`DELAY` ticks ahead of the sim).
    pub fn push_local(&mut self, input: u8) {
        self.streams[self.me].push(input);
    }

    /// How many ticks have our input.
    pub fn local_ticks(&self) -> usize {
        self.streams[self.me].len()
    }

    /// Every car's input for tick `t` (0-based), in car order, once the host has them all.
    pub fn inputs(&self, t: usize) -> Option<Vec<u8>> {
        self.frames.get(t).cloned()
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

    fn check(&mut self, theirs: Option<(u32, u64)>) {
        if let Some((t, h)) = theirs
            && let Some(&(_, mine)) = self.hashes.iter().find(|x| x.0 == t as u64)
            && mine != h
        {
            self.desync.get_or_insert(t as u64);
        }
    }

    /// Receives what's there and sends what the others lack. Call once per frame.
    pub fn poll(&mut self) {
        for (from, msg) in received(&self.sock) {
            let Some(k) = self.peers.iter().position(|p| p.addr == from) else {
                continue;
            };
            // the host's guest k is car k + 1
            let car = if self.host() { k + 1 } else { 0 };
            if self.gone[car] {
                continue;
            }
            self.peers[k].heard = Instant::now();
            match msg {
                Msg::Inputs {
                    have,
                    first,
                    bytes,
                    hash,
                } if self.host() => {
                    let p = &mut self.peers[k];
                    p.start = None; // it's running: it got Start
                    p.has = p.has.max(have as usize);
                    let stream = &mut self.streams[car];
                    for (i, b) in bytes.into_iter().enumerate() {
                        if first as usize + i == stream.len() {
                            stream.push(b);
                        }
                    }
                    self.check(hash);
                }
                Msg::Frames {
                    have,
                    first,
                    players,
                    bytes,
                    gone,
                    hash,
                } if !self.host() && players as usize == self.players => {
                    self.peers[0].has = self.peers[0].has.max(have as usize);
                    for (i, f) in bytes.chunks_exact(self.players).enumerate() {
                        if first as usize + i == self.frames.len() {
                            self.frames.push(f.to_vec());
                        }
                    }
                    for (c, g) in self.gone.iter_mut().enumerate() {
                        *g |= gone & (1 << c) != 0;
                    }
                    self.check(hash);
                }
                Msg::Bye if self.host() => self.gone[car] = true,
                Msg::Bye => self.lost = true,
                _ => {}
            }
        }
        // silence: a guest is gone; the host is the race
        for k in 0..self.peers.len() {
            if self.peers[k].heard.elapsed() > TIMEOUT {
                match self.host() {
                    true => self.gone[k + 1] = true,
                    false => self.lost = true,
                }
            }
        }
        if self.host() {
            if self.gone[1..].iter().all(|&g| g) {
                self.lost = true; // everyone left: back to driving alone
            }
            // complete ticks: everyone's input in, a gone car's as far as it got, then none
            loop {
                let t = self.frames.len();
                let ready = (0..self.players).all(|c| self.gone[c] || t < self.streams[c].len());
                if !ready {
                    break;
                }
                let frame = (0..self.players)
                    .map(|c| self.streams[c].get(t).copied().unwrap_or(0))
                    .collect();
                self.frames.push(frame);
            }
        }
        // ponytail: one packet per peer per frame (60/s), resent in full until acknowledged
        let hash = self.hashes.last().map(|&(t, h)| (t as u32, h));
        let gone = (0..self.players).fold(0u8, |m, c| m | (self.gone[c] as u8) << c);
        for k in 0..self.peers.len() {
            let car = if self.host() { k + 1 } else { 0 };
            if self.host() && self.gone[car] {
                continue;
            }
            let p = &self.peers[k];
            if let Some(s) = &p.start
                && self.sent.is_none_or(|t| t.elapsed() > RESEND)
            {
                send(&self.sock, p.addr, &Msg::Start(s.clone()));
            }
            let msg = if self.host() {
                let from = p.has.min(self.frames.len());
                let to = (from + BATCH).min(self.frames.len());
                Msg::Frames {
                    have: self.streams[car].len() as u32,
                    first: from as u32,
                    players: self.players as u8,
                    bytes: self.frames[from..to].concat(),
                    gone,
                    hash,
                }
            } else {
                let mine = &self.streams[self.me];
                let from = p.has.min(mine.len());
                Msg::Inputs {
                    have: self.frames.len() as u32,
                    first: from as u32,
                    bytes: mine[from..(from + BATCH).min(mine.len())].to_vec(),
                    hash,
                }
            };
            send(&self.sock, p.addr, &msg);
        }
        self.sent = Some(Instant::now());
    }

    /// Tells the others we're leaving.
    pub fn bye(&self) {
        for p in &self.peers {
            for _ in 0..3 {
                send(&self.sock, p.addr, &Msg::Bye);
            }
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

    fn start(n: usize) -> Start {
        Start {
            map: "terep2".into(),
            map_hash: 0x0123_4567_89ab_cdef,
            soft: n % 2 == 0,
            gradual: n > 2,
            cars: (0..n as u8).collect(),
            skins: (0..n).map(|i| format!("S{i}.PCX")).collect(),
            spawns: (0..n as u16)
                .map(|i| [0x8000 + 0x180 * i, 0x7a00, 0x4000])
                .collect(),
            you: n as u8 - 1,
        }
    }

    #[test]
    fn messages_round_trip() {
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
            Msg::Lobby { players: 3 },
            Msg::Start(start(2)),
            Msg::Start(start(4)),
            Msg::Refused("full".into()),
            Msg::Inputs {
                have: 9,
                first: 4,
                bytes: vec![1, 5, 33],
                hash: Some((120, 77)),
            },
            Msg::Frames {
                have: 12,
                first: 4,
                players: 3,
                bytes: vec![1, 2, 3, 4, 5, 6],
                gone: 0b100,
                hash: None,
            },
            Msg::Bye,
        ] {
            assert_eq!(Msg::decode(&m.encode()), Some(m));
        }
        assert_eq!(Msg::decode(b"RFN1\x05\x01"), None, "truncated");
        assert_eq!(Msg::decode(b"HTTP/1.1"), None, "not ours");
        let ragged = Msg::Frames {
            have: 0,
            first: 0,
            players: 3,
            bytes: vec![1, 2],
            gone: 0,
            hash: None,
        };
        assert_eq!(
            Msg::decode(&ragged.encode()),
            None,
            "frames not a whole tick"
        );
    }

    /// What car `me` presses on tick `t`.
    fn keys(me: usize, t: usize) -> u8 {
        run::input(
            Keys {
                throttle: (t + me * 37) % 300 < 220,
                left: me % 2 == 0 && t % (200 + me * 30) < 30,
                right: me % 2 == 1 && t % 260 < 40,
                ..Keys::default()
            },
            t == 400 + me,
            (500..700).contains(&t),
            t == 800 + me,
        )
    }

    /// `n` machines over localhost UDP, the host and `n - 1` joiners: they meet in the lobby,
    /// start, and run 6 s in lockstep, each with its own sim. `quit` = (car, tick): that player
    /// stops there without a word (their car then drives on with no keys).
    /// Returns every machine's final state hash, and each car's inputs as the host ran them.
    fn race(n: usize, quit: Option<(usize, usize)>) -> (Vec<u64>, Vec<Vec<u8>>) {
        let read = |f: &str| crate::assets::read(f).unwrap();
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let terrain =
            Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap();
        let new_sim = || {
            let mut s = Sim::new(terrain.clone());
            s.gradual_repair = n % 2 == 1; // F mends while held on odd counts
            for i in 0..n as u16 {
                s.cars.push(SimCar::spawn_facing(
                    &car,
                    &s.terrain,
                    0x8000 + 0x180 * i,
                    0x7a00,
                    0,
                ));
            }
            s
        };
        let mut host = Host::bind(0).unwrap();
        let addr: SocketAddr = (Ipv4Addr::LOCALHOST, host.port()).into();
        let mut joiners: Vec<_> = (1..n)
            .map(|i| Some(Joiner::new(addr, i as u8, "").unwrap()))
            .collect();
        // the lobby: everyone in, in order (each waits for the one before, so the slots are known)
        let t0 = Instant::now();
        for (i, j) in joiners.iter_mut().enumerate() {
            while host.guests.len() < i + 1 {
                assert!(t0.elapsed() < Duration::from_secs(3), "lobby");
                let j = j.as_mut().unwrap();
                assert!(matches!(j.poll(), JoinState::Waiting));
                host.poll("terep2");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(
            host.guests.iter().map(|g| g.car).collect::<Vec<_>>(),
            (1..n as u8).collect::<Vec<_>>()
        );
        let mut s = start(n);
        s.cars = vec![0; n];
        let mut links = vec![host.start(s.clone())];
        let mut pending: Vec<Option<Link>> = (1..n).map(|_| None).collect();
        while pending.iter().any(Option::is_none) {
            assert!(t0.elapsed() < Duration::from_secs(5), "start");
            links[0].poll();
            for (i, j) in joiners.iter_mut().enumerate() {
                if let Some(jj) = j
                    && let JoinState::Started(st) = jj.poll()
                {
                    assert_eq!((st.you as usize, &st.cars), (i + 1, &s.cars));
                    pending[i] = Some(j.take().unwrap().start(&st));
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        links.extend(pending.into_iter().map(Option::unwrap));

        let ticks = 6 * crate::sim::TICK_HZ as usize;
        let mut sides: Vec<_> = links.into_iter().map(|l| (l, new_sim(), 0usize)).collect();
        let mut ran: Vec<Vec<u8>> = vec![vec![]; n]; // the host's frames, as run
        let t0 = Instant::now();
        let running = |c: usize, t: usize| quit.is_none_or(|(q, at)| c != q || t < at);
        while sides
            .iter()
            .enumerate()
            .any(|(c, s)| running(c, s.2) && s.2 < ticks)
        {
            assert!(t0.elapsed() < Duration::from_secs(30), "lockstep stalled");
            for (c, (link, sim, t)) in sides.iter_mut().enumerate() {
                if !running(c, *t) {
                    continue; // this player walked away
                }
                link.poll();
                while link.local_ticks() < *t + DELAY + 1 {
                    let me = link.me;
                    let k = link.local_ticks() - DELAY;
                    link.push_local(keys(me, k));
                }
                while *t < ticks
                    && let Some(inputs) = link.inputs(*t)
                {
                    let mut races: Vec<_> = (0..n).map(|_| Race::new(None)).collect();
                    let models = vec![&car; n];
                    run::tick_all(sim, &models, None, &mut races, &inputs, *t as u64);
                    if c == 0 {
                        inputs.iter().enumerate().for_each(|(k, &b)| ran[k].push(b));
                    }
                    *t += 1;
                    link.ran(*t as u64, || sim.state_hash());
                }
                assert_eq!(link.desync, None, "car {c}");
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        for _ in 0..5 {
            sides.iter_mut().for_each(|s| s.0.poll());
        }
        if quit.is_none() {
            assert!(
                sides
                    .iter()
                    .all(|s| !s.0.lost && !s.0.gone.iter().any(|&g| g))
            );
        }
        let hashes = sides
            .iter()
            .enumerate()
            .filter(|&(c, s)| running(c, s.2))
            .map(|(_, s)| s.1.state_hash())
            .collect();
        // a reference sim fed the host's frames agrees with it
        let mut reference = new_sim();
        for t in 0..ticks {
            let inputs: Vec<u8> = (0..n).map(|c| ran[c][t]).collect();
            let mut races: Vec<_> = (0..n).map(|_| Race::new(None)).collect();
            run::tick_all(
                &mut reference,
                &vec![&car; n],
                None,
                &mut races,
                &inputs,
                t as u64,
            );
        }
        assert_eq!(reference.state_hash(), sides[0].1.state_hash());
        (hashes, ran)
    }

    /// Four machines stay bit-identical, and what they ran is every player's keys, each `DELAY`
    /// ticks late.
    #[test]
    fn four_machines_stay_in_lockstep() {
        crate::need_assets!();
        let (hashes, ran) = race(4, None);
        assert_eq!(hashes.len(), 4);
        assert!(hashes.iter().all(|&h| h == hashes[0]), "{hashes:?}");
        for (c, inputs) in ran.iter().enumerate() {
            for (t, &b) in inputs.iter().enumerate() {
                let want = if t < DELAY { 0 } else { keys(c, t - DELAY) };
                assert_eq!(b, want, "car {c} tick {t}");
            }
        }
    }

    /// Two still work (the most common race).
    #[test]
    fn two_machines_stay_in_lockstep() {
        crate::need_assets!();
        let (hashes, _) = race(2, None);
        assert_eq!(hashes[0], hashes[1]);
    }

    /// One of three walks away mid-race without a word: after the timeout the host gives their
    /// car no keys, and the two left carry on in step.
    #[test]
    fn a_player_dropping_out_doesnt_stop_the_race() {
        crate::need_assets!();
        let (hashes, ran) = race(3, Some((2, 200)));
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], hashes[1]);
        assert!(
            ran[2][250..].iter().all(|&b| b == 0),
            "the gone car gets no keys"
        );
        assert!(
            ran[0][250..].iter().any(|&b| b != 0),
            "the others still drive"
        );
    }
}
