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
//!
//! A dedicated host (`reform serve`, #62) drives no car: every slot is a joiner's, car 0 the
//! first in. It ends each round with `RoundOver` (the standings), and the joiners come back to
//! its lobby by themselves for the next map. `Start` carries the host's input delay. Its rounds
//! have `MAX_PLAYERS` slots: someone who joins mid-round (#63) gets a free one, from the next
//! tick the host completes (`Late`, sent to everyone with the frames). They get the round's
//! Start and every frame so far, replay them, and the host waits for them once their inputs
//! run ahead of the frames; until then their car stands on the grid with no keys. With no free
//! slot they wait for the next round's lobby.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const PORT: u16 = 47800;
/// Cars in an online race at most: lockstep waits for the slowest link, so more wants a different
/// netcode (rollback).
pub const MAX_PLAYERS: usize = 4;
/// Ticks between pressing a key and its tick: 6 = 50 ms, enough for a LAN over Wi-Fi (4 stalled
/// with 8–16 ms each way, `REFORMERS_LAG=8+8`). A dedicated host may choose more (`Start::delay`).
pub const DELAY: usize = 6;
const HASH_EVERY: u64 = 120;
const TIMEOUT: Duration = Duration::from_secs(5);
const RESEND: Duration = Duration::from_millis(200);
const MAGIC: &[u8; 4] = b"RFN1";
/// Raised whenever two builds would disagree on the messages or the sim (7: `Start::snow` and `snow_grip`, #68).
const VERSION: u8 = 7;
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
    /// Snow builds up on the map (#68), sent in the soft byte's bit 3. The host decides it.
    pub snow: bool,
    /// Snow drives slippery (`Sim::snow_grip`).
    pub snow_grip: bool,
    /// Per car (0 = host): model 0–4, skin file ("" = own), spawn x, y and heading.
    pub cars: Vec<u8>,
    pub skins: Vec<String>,
    pub spawns: Vec<[u16; 3]>,
    /// Which car is this machine's.
    pub you: u8,
    /// The host drives no car (`reform serve`): rounds end with `RoundOver`, then the next map.
    /// Its rounds have `MAX_PLAYERS` slots, the ones past `cars` for latecomers (`Late`).
    pub dedicated: bool,
    /// Ticks between a key and its tick, `DELAY` unless a dedicated host says otherwise.
    pub delay: u8,
}

/// A car that joined a dedicated host's round at tick `at`: it's car `Start::cars.len()` plus
/// its place in the list, spawned before tick `at` runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Late {
    pub at: u64,
    pub car: u8,
    pub skin: String,
    pub spawn: [u16; 3],
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
    /// Host → joiner waiting to start: you're in, `players` cars so far (host included). 0 from
    /// a dedicated host: a round is on, keep asking and you're in the next one.
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
    /// this joiner's inputs; bit k of `gone` = car k left; every latecomer so far.
    Frames {
        have: u32,
        first: u32,
        players: u8,
        bytes: Vec<u8>,
        gone: u8,
        late: Vec<Late>,
        hash: Option<(u32, u64)>,
    },
    Bye,
    /// Dedicated host → joiners: the round is over (the standings); come back to the lobby.
    RoundOver(String),
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
                    s.soft as u8
                        | (s.gradual as u8) << 1
                        | (s.dedicated as u8) << 2
                        | (s.snow as u8) << 3
                        | (s.snow_grip as u8) << 4,
                    s.delay,
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
            Msg::RoundOver(text) => {
                o.push(9);
                put_str(&mut o, text);
            }
            Msg::Lobby { players } => o.extend([7, *players]),
            Msg::Frames {
                have,
                first,
                players,
                bytes,
                gone,
                late,
                hash,
            } => {
                o.push(8);
                o.extend(have.to_le_bytes());
                o.extend(first.to_le_bytes());
                o.extend([*players, *gone, late.len() as u8]);
                for l in late {
                    o.extend((l.at as u32).to_le_bytes());
                    o.push(l.car);
                    put_str(&mut o, &l.skin);
                    l.spawn.iter().for_each(|v| o.extend(v.to_le_bytes()));
                }
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
                let (flags, delay) = (r.u8()?, r.u8()?);
                let (you, n) = (r.u8()?, r.u8()? as usize);
                let (soft, gradual, dedicated) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
                let (snow, snow_grip) = (flags & 8 != 0, flags & 16 != 0);
                let slots = if dedicated { MAX_PLAYERS } else { n };
                if n > MAX_PLAYERS || you as usize >= slots || delay == 0 {
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
                    snow,
                    snow_grip,
                    cars,
                    skins,
                    spawns,
                    you,
                    dedicated,
                    delay,
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
            9 => Msg::RoundOver(r.str()?),
            7 => Msg::Lobby { players: r.u8()? },
            8 => {
                let (have, first) = (r.u32()?, r.u32()?);
                let (players, gone) = (r.u8()?, r.u8()?);
                let mut late = vec![];
                for _ in 0..r.u8()? {
                    late.push(Late {
                        at: r.u32()? as u64,
                        car: r.u8()?,
                        skin: r.str()?,
                        spawn: [r.u16()?, r.u16()?, r.u16()?],
                    });
                }
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
                    late,
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
    let bytes = msg.encode();
    if let Some((ms, jitter)) = lag()
        && let Ok(sock) = sock.try_clone()
    {
        use std::hash::{BuildHasher, RandomState};
        let ms = ms + RandomState::new().hash_one(0u8) % (jitter + 1);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = sock.send_to(&bytes, to);
        });
        return;
    }
    let _ = sock.send_to(&bytes, to); // a lost packet is resent by the next
}

/// `REFORMERS_LAG=ms` or `ms+jitter` (e.g. `20+15`): every packet we send goes out that much
/// later (jitter: up to that much more, at random, so they also arrive out of order), to try a
/// bad network on one machine.
fn lag() -> Option<(u64, u64)> {
    static LAG: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    *LAG.get_or_init(|| {
        let v = std::env::var("REFORMERS_LAG").ok()?;
        let (ms, jitter) = v.split_once('+').unwrap_or((&v, "0"));
        Some((ms.trim().parse().ok()?, jitter.trim().parse().ok()?))
    })
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
    /// In the order they joined: they'll be cars 1, 2, … (0, 1, … on a dedicated host).
    pub guests: Vec<Guest>,
    /// Drives no car (`reform serve`): every slot is a guest's.
    pub dedicated: bool,
}

impl Host {
    pub fn bind(port: u16) -> std::io::Result<Host> {
        Ok(Host {
            sock: socket(port)?,
            guests: vec![],
            dedicated: false,
        })
    }

    /// A host without a car of its own (`reform serve`).
    pub fn bind_dedicated(port: u16) -> std::io::Result<Host> {
        Ok(Host {
            dedicated: true,
            ..Host::bind(port)?
        })
    }

    /// Cars in the lobby so far, the host's own included.
    pub fn players(&self) -> usize {
        self.guests.len() + !self.dedicated as usize
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
                    if !known && self.players() >= MAX_PLAYERS {
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
                    let players = self.players() as u8;
                    send(&self.sock, from, &Msg::Lobby { players });
                }
                Msg::Bye => self.guests.retain(|g| g.addr != from),
                _ => {}
            }
        }
    }

    /// The race starts: `start` lists every car (the host's first unless dedicated, then the
    /// guests' in order; its `you` and `dedicated` are set here). Each guest gets it with their
    /// own slot, repeated until their first inputs show it arrived.
    pub fn start(self, mut start: Start) -> Link {
        start.dedicated = self.dedicated;
        let first = !self.dedicated as usize; // the guests' first car
        let peers = self
            .guests
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let mut s = start.clone();
                s.you = (first + i) as u8;
                Peer::new(g.addr, first + i, Some(s))
            })
            .collect();
        let me = if self.dedicated { None } else { Some(0) };
        Link::new(self.sock, peers, true, me, &start)
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
        let host = Peer::new(self.host, 0, None);
        Link::new(
            self.sock,
            vec![host],
            false,
            Some(start.you as usize),
            start,
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

/// The other end: for the host, each guest; for a joiner, the host.
struct Peer {
    addr: SocketAddr,
    /// The guest's car (for a joiner's host: unused).
    car: usize,
    /// How many of what we send it (host: frames; joiner: our inputs) it has.
    has: usize,
    heard: Instant,
    /// Host: the Start to repeat until this guest's first packet, and when it last went.
    start: Option<Start>,
    start_sent: Option<Instant>,
}

impl Peer {
    fn new(addr: SocketAddr, car: usize, start: Option<Start>) -> Peer {
        Peer {
            addr,
            car,
            has: 0,
            heard: Instant::now(),
            start,
            start_sent: None,
        }
    }
}

/// A running race: every car's inputs, tick by tick.
pub struct Link {
    sock: UdpSocket,
    peers: Vec<Peer>,
    /// We decide the frames (and hand out the slots).
    host: bool,
    /// Our car: 0 host, 1… joiners (a dedicated host has none: `usize::MAX`).
    pub me: usize,
    /// Cars in the race.
    pub players: usize,
    /// Ticks between a key and its tick (`Start::delay`).
    pub delay: usize,
    /// Inputs by car, then tick (host: everyone's as they arrive; joiner: only ours, in
    /// `streams[me]`). The first `delay` ticks run on no keys.
    streams: Vec<Vec<u8>>,
    /// Complete ticks, a byte per car: what the sim runs.
    frames: Vec<Vec<u8>>,
    /// Cars whose player left (they get no keys from where their inputs ran out).
    pub gone: Vec<bool>,
    /// What the round started from (a dedicated host hands it to latecomers).
    start: Start,
    /// Cars that joined mid-round (dedicated host only), in slot order.
    pub late: Vec<Late>,
    /// Dedicated host: where each slot starts, latecomers' included (the game's grid).
    pub spawns: Vec<[u16; 3]>,
    /// Host: the slots it waits for. An empty slot, or a latecomer still catching up, gets no
    /// keys until its inputs run ahead of the frames.
    live: Vec<bool>,
    /// Our recent state hashes (tick, hash).
    hashes: Vec<(u64, u64)>,
    /// Set when the sims disagree (the tick), or the race is over for us: the host left (or,
    /// for the host, every guest did).
    pub desync: Option<u64>,
    pub lost: bool,
    /// A dedicated host ended the round: the standings it sent.
    pub over: Option<String>,
}

impl Link {
    /// `me`: our car, None for a dedicated host.
    fn new(
        sock: UdpSocket,
        peers: Vec<Peer>,
        host: bool,
        me: Option<usize>,
        start: &Start,
    ) -> Link {
        let n = start.cars.len();
        let players = if start.dedicated { MAX_PLAYERS } else { n };
        let delay = start.delay as usize;
        Link {
            sock,
            host,
            peers,
            me: me.unwrap_or(usize::MAX),
            players,
            delay,
            streams: vec![vec![0; delay]; players],
            frames: (0..delay).map(|_| vec![0; players]).collect(),
            gone: vec![false; players],
            start: start.clone(),
            late: vec![],
            spawns: start.spawns.clone(),
            live: (0..players).map(|c| c < n).collect(),
            hashes: vec![],
            desync: None,
            lost: false,
            over: None,
        }
    }

    /// We're the host (decide the frames), with a car of our own or not.
    pub fn host(&self) -> bool {
        self.host
    }

    /// Where the host is (a joiner's only peer).
    pub fn host_addr(&self) -> SocketAddr {
        self.peers[0].addr
    }

    /// Our input for the next tick that hasn't got one (`delay` ticks ahead of the sim).
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

    /// How many ticks are complete: a latecomer far behind them is catching up.
    pub fn ready(&self) -> usize {
        self.frames.len()
    }

    /// The cars that join just before tick `t` runs, with their car numbers.
    pub fn joining(&self, t: u64) -> impl Iterator<Item = (usize, &Late)> {
        let n = self.start.cars.len();
        self.late
            .iter()
            .enumerate()
            .filter(move |(_, l)| l.at == t)
            .map(move |(i, l)| (n + i, l))
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
                // a dedicated host takes newcomers into a free slot (they catch up on the frames
                // so far), or keeps them waiting for the next round's lobby
                if self.me == usize::MAX
                    && let Msg::Join { version, car, skin } = msg
                {
                    let slot = self.start.cars.len() + self.late.len();
                    if version != VERSION {
                        let why = "the host runs a different version".into();
                        send(&self.sock, from, &Msg::Refused(why));
                    } else if slot >= self.players.min(self.spawns.len()) {
                        send(&self.sock, from, &Msg::Lobby { players: 0 });
                    } else {
                        self.late.push(Late {
                            at: self.frames.len() as u64,
                            car: car.min(4),
                            skin,
                            spawn: self.spawns[slot],
                        });
                        let mut s = self.start.clone();
                        s.you = slot as u8;
                        self.peers.push(Peer::new(from, slot, Some(s)));
                    }
                }
                continue;
            };
            let car = self.peers[k].car;
            if self.host() && self.gone[car] {
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
                    // a latecomer's inputs ran ahead of the frames: it has caught up
                    self.live[car] |= stream.len() > self.frames.len();
                    self.check(hash);
                }
                Msg::Frames {
                    have,
                    first,
                    players,
                    bytes,
                    gone,
                    late,
                    hash,
                } if !self.host() && players as usize == self.players => {
                    if late.len() > self.late.len() {
                        self.late = late;
                    }
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
                Msg::RoundOver(text) if !self.host() => self.over = Some(text),
                _ => {}
            }
        }
        // silence: a guest is gone; the host is the race
        for k in 0..self.peers.len() {
            if self.peers[k].heard.elapsed() > TIMEOUT {
                match self.host() {
                    true => self.gone[self.peers[k].car] = true,
                    false => self.lost = true,
                }
            }
        }
        if self.host() {
            if self.peers.iter().all(|p| self.gone[p.car]) {
                self.lost = true; // everyone left: back to driving alone (or the lobby)
            }
            // complete ticks: everyone's input in, a gone car's as far as it got, then none; a
            // slot that isn't live gets none, kept in its stream so a latecomer's line up after
            loop {
                let t = self.frames.len();
                let ready = (0..self.players)
                    .all(|c| self.gone[c] || !self.live[c] || t < self.streams[c].len());
                if !ready {
                    break;
                }
                for c in 0..self.players {
                    if !self.live[c] && self.streams[c].len() <= t {
                        self.streams[c].resize(t + 1, 0);
                    }
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
            let car = self.peers[k].car;
            if self.host() && self.gone[car] {
                continue;
            }
            let p = &mut self.peers[k];
            if let Some(s) = &p.start
                && p.start_sent.is_none_or(|t| t.elapsed() > RESEND)
            {
                send(&self.sock, p.addr, &Msg::Start(s.clone()));
                p.start_sent = Some(Instant::now());
            }
            let p = &self.peers[k];
            let msg = if self.host() {
                let from = p.has.min(self.frames.len());
                let to = (from + BATCH).min(self.frames.len());
                Msg::Frames {
                    have: self.streams[car].len() as u32,
                    first: from as u32,
                    players: self.players as u8,
                    bytes: self.frames[from..to].concat(),
                    gone,
                    late: self.late.clone(),
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
    }

    /// Tells the others we're leaving.
    pub fn bye(&self) {
        for p in &self.peers {
            for _ in 0..3 {
                send(&self.sock, p.addr, &Msg::Bye);
            }
        }
    }

    /// Dedicated host: the round is over, with `standings`; back to an empty lobby on the same
    /// port, which the joiners rejoin by themselves.
    pub fn end_round(self, standings: &str) -> Host {
        for p in &self.peers {
            for _ in 0..3 {
                send(&self.sock, p.addr, &Msg::RoundOver(standings.into()));
            }
        }
        Host {
            sock: self.sock,
            guests: vec![],
            dedicated: true,
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
            snow: n == 4,
            snow_grip: n == 2,
            cars: (0..n as u8).collect(),
            skins: (0..n).map(|i| format!("S{i}.PCX")).collect(),
            spawns: (0..n as u16)
                .map(|i| [0x8000 + 0x180 * i, 0x7a00, 0x4000])
                .collect(),
            you: n as u8 - 1,
            dedicated: n == 3,
            delay: DELAY as u8 + n as u8,
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
                late: vec![Late {
                    at: 900,
                    car: 2,
                    skin: "HUFO.PCX".into(),
                    spawn: [1, 2, 3],
                }],
                hash: None,
            },
            Msg::Bye,
            Msg::RoundOver("1. P2  3 laps".into()),
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
            late: vec![],
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
        s.delay = DELAY as u8;
        let mut links = vec![host.start(s.clone())];
        let mut pending: Vec<Option<Link>> = (1..n).map(|_| None).collect();
        while pending.iter().any(Option::is_none) {
            assert!(t0.elapsed() < Duration::from_secs(5), "start");
            links[0].poll();
            for (i, j) in joiners.iter_mut().enumerate() {
                if let Some(jj) = j
                    && let JoinState::Started(st) = jj.poll()
                {
                    assert_eq!(
                        (st.you as usize, &st.cars, st.dedicated),
                        (i + 1, &s.cars, false)
                    );
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

    /// A dedicated host (#62): two joiners get cars 0 and 1 and the host's delay, the host runs
    /// the same sim without a car of its own, and after `end_round` both come back to its lobby
    /// by themselves and get the next start.
    #[test]
    fn a_dedicated_host_runs_rounds() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let terrain =
            Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap();
        let new_sim = || {
            let mut s = Sim::new(terrain.clone());
            for i in 0..2 {
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
        let mut host = Host::bind_dedicated(0).unwrap();
        let addr: SocketAddr = (Ipv4Addr::LOCALHOST, host.port()).into();
        let mut s = start(2);
        (s.cars, s.delay) = (vec![0; 2], 7);
        let t0 = Instant::now();
        // two rounds: the second is set up by the joiners coming back on their own
        let mut back: Vec<SocketAddr> = vec![addr; 2];
        // and a latecomer, who connects mid-round and waits for the next lobby
        let mut late = Joiner::new(addr, 0, "").unwrap();
        for round in 0..2 {
            let mut joiners: Vec<_> = back
                .iter()
                .map(|&a| Some(Joiner::new(a, 0, "").unwrap()))
                .collect();
            for i in 0..2 {
                while host.guests.len() < i + 1 {
                    assert!(
                        t0.elapsed() < Duration::from_secs(5),
                        "lobby, round {round}"
                    );
                    assert!(matches!(
                        joiners[i].as_mut().unwrap().poll(),
                        JoinState::Waiting
                    ));
                    host.poll("terep2");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            assert_eq!(host.players(), 2, "the host has no car");
            let mut server = host.start(s.clone());
            assert!(server.host() && server.peers.iter().map(|p| p.car).eq(0..2));
            let mut links: Vec<Option<Link>> = vec![None, None];
            while links.iter().any(Option::is_none) {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "start, round {round}"
                );
                server.poll();
                for (i, j) in joiners.iter_mut().enumerate() {
                    if let Some(jj) = j
                        && let JoinState::Started(st) = jj.poll()
                    {
                        assert_eq!((st.you as usize, st.dedicated, st.delay), (i, true, 7));
                        links[i] = Some(j.take().unwrap().start(&st));
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut links: Vec<Link> = links.into_iter().map(Option::unwrap).collect();
            assert!(links.iter().all(|l| l.delay == 7));
            // 2 s in lockstep: the joiners drive, the host only runs the ticks
            let ticks = 2 * crate::sim::TICK_HZ as usize;
            let mut sims = vec![new_sim(), new_sim(), new_sim()]; // the host's last
            let mut t = [0usize; 3];
            while t.iter().any(|&t| t < ticks) {
                assert!(t0.elapsed() < Duration::from_secs(30), "lockstep stalled");
                for (c, link) in links.iter_mut().enumerate() {
                    link.poll();
                    while link.local_ticks() < t[c] + link.delay + 1 {
                        let k = link.local_ticks() - link.delay;
                        link.push_local(keys(c, k));
                    }
                }
                server.poll();
                for c in 0..3 {
                    let link = if c < 2 { &mut links[c] } else { &mut server };
                    while t[c] < ticks
                        && let Some(inputs) = link.inputs(t[c])
                    {
                        let mut races: Vec<_> = (0..2).map(|_| Race::new(None)).collect();
                        run::tick_all(
                            &mut sims[c],
                            &[&car, &car],
                            None,
                            &mut races,
                            &inputs,
                            t[c] as u64,
                        );
                        t[c] += 1;
                        link.ran(t[c] as u64, || sims[c].state_hash());
                    }
                    assert_eq!(link.desync, None, "side {c}, round {round}");
                }
                if round == 0 {
                    assert!(matches!(late.poll(), JoinState::Waiting));
                }
                std::thread::sleep(Duration::from_micros(200));
            }
            let h = sims[2].state_hash();
            assert!(sims.iter().all(|s| s.state_hash() == h));
            host = server.end_round("1. P1");
            for link in &mut links {
                while link.over.is_none() {
                    assert!(t0.elapsed() < Duration::from_secs(30), "round over");
                    link.poll();
                }
                assert_eq!(link.over.as_deref(), Some("1. P1"));
            }
            if round == 0 {
                assert_eq!(late.lobby, Some(0), "told a round is on");
                while host.guests.is_empty() {
                    assert!(t0.elapsed() < Duration::from_secs(30), "the latecomer");
                    late.poll();
                    host.poll("terep2");
                }
                assert_eq!(
                    host.guests[0].addr.port(),
                    late.sock.local_addr().unwrap().port()
                );
                host.guests.clear(); // ponytail: the test's next round is for two
            }
            back = links.iter().map(Link::host_addr).collect();
        }
    }

    #[test]
    fn a_latecomer_catches_up_and_drives() {
        crate::need_assets!();
        let read = |f: &str| crate::assets::read(f).unwrap();
        let car = crate::car::parse(&read("CAR1.DAT")).unwrap();
        let terrain =
            Terrain::load(&read("maps/terep2/MAP.PCX"), &read("maps/terep2/COL.PCX")).unwrap();
        let spot = |i: u16| [0x8000 + 0x180 * i, 0x7a00, 0];
        let mut host = Host::bind_dedicated(0).unwrap();
        let addr: SocketAddr = (Ipv4Addr::LOCALHOST, host.port()).into();
        let t0 = Instant::now();
        let mut first = Joiner::new(addr, 0, "").unwrap();
        while host.guests.is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(5), "lobby");
            first.poll();
            host.poll("terep2");
        }
        let mut s = start(1);
        (s.cars, s.skins, s.spawns, s.delay) = (vec![0], vec!["".into()], vec![spot(0)], 6);
        let mut server = host.start(s);
        server.spawns = (0..2).map(spot).collect(); // room for one more
        // each side: its link, sim, races and next tick; a new car goes down just before its tick
        struct Side {
            link: Link,
            sim: Sim,
            races: Vec<Race>,
            t: usize,
        }
        let side = |link: Link, st: &Start| Side {
            sim: {
                let mut sim = Sim::new(terrain.clone());
                for &[x, y, h] in &st.spawns {
                    sim.cars
                        .push(SimCar::spawn_facing(&car, &sim.terrain, x, y, h));
                }
                sim
            },
            races: st.cars.iter().map(|_| Race::new(None)).collect(),
            link,
            t: 0,
        };
        let run = |s: &mut Side, upto: usize| {
            while s.t < upto
                && let Some(inputs) = s.link.inputs(s.t)
            {
                let joining: Vec<[u16; 3]> =
                    s.link.joining(s.t as u64).map(|(_, l)| l.spawn).collect();
                for [x, y, h] in joining {
                    s.sim
                        .cars
                        .push(SimCar::spawn_facing(&car, &s.sim.terrain, x, y, h));
                    s.races.push(Race::new(None));
                }
                let n = s.sim.cars.len();
                let models = vec![&car; n];
                run::tick_all(
                    &mut s.sim,
                    &models,
                    None,
                    &mut s.races,
                    &inputs[..n],
                    s.t as u64,
                );
                s.t += 1;
                s.link.ran(s.t as u64, || s.sim.state_hash());
            }
        };
        let st = loop {
            assert!(t0.elapsed() < Duration::from_secs(5), "start");
            server.poll();
            if let JoinState::Started(st) = first.poll() {
                break st;
            }
        };
        let mut sides = vec![side(first.start(&st), &st)];
        let mut srv = side(server, &st);
        let mut late = Some(Joiner::new(addr, 0, "").unwrap());
        let end = 3 * crate::sim::TICK_HZ as usize;
        let mut joined_at = None;
        while sides.iter().chain([&srv]).any(|s| s.t < end) || sides.len() < 2 {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "stalled at {}",
                srv.t
            );
            for s in sides.iter_mut() {
                s.link.poll();
                while s.link.local_ticks() < s.t + s.link.delay + 1 {
                    let k = s.link.local_ticks() - s.link.delay;
                    s.link.push_local(keys(s.link.me, k));
                }
                run(s, end);
            }
            srv.link.poll();
            run(&mut srv, end);
            // a second's racing in, someone else joins
            if srv.t >= crate::sim::TICK_HZ as usize
                && let Some(j) = &mut late
            {
                match j.poll() {
                    JoinState::Started(st) => {
                        assert_eq!((st.you, st.dedicated, st.cars.len()), (1, true, 1));
                        let link = late.take().unwrap().start(&st);
                        sides.push(side(link, &st));
                    }
                    JoinState::Waiting => {}
                    JoinState::Failed(why) => panic!("{why}"),
                }
            }
            if let Some(l) = srv.link.late.first() {
                joined_at.get_or_insert(l.at);
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        for s in &sides {
            assert_eq!(s.link.desync, None);
        }
        let at = joined_at.expect("the latecomer got a slot") as usize;
        assert!(
            at >= crate::sim::TICK_HZ as usize && at < end,
            "joined at {at}"
        );
        assert_eq!(sides[1].link.late, srv.link.late, "everyone knows when");
        assert!(sides.iter().all(|s| s.sim.cars.len() == 2));
        let h = srv.sim.state_hash();
        assert!(sides.iter().all(|s| s.sim.state_hash() == h), "same race");
        // and the latecomer drove: its car left the grid
        let [x, y, _] = srv.sim.cars[1].centre();
        let [gx, gy, _] = spot(1);
        assert!(
            (x - gx as f32).abs() + (y - gy as f32).abs() > 256.0,
            "car 1 moved"
        );
    }
}
