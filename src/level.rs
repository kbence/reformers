//! A map folder as the game and the dedicated server load it: terrain, time-trial track, trees
//! and the hash that says two machines have the same files.

use crate::sim::Trunk;
use crate::terrain::{SIZE, Terrain};
use crate::track::{Track, Zone};
use crate::{assets, objects, pcx};

/// Where cars start without CHECK.PCX.
pub const SPAWN: (u16, u16) = (0x8000, 0x7a00);

/// Car `i`'s place on the starting grid, all facing `heading`: two side by side (the second 1.5
/// cells to the right of `spawn`), each further row 2.5 cells back.
pub fn grid(spawn: (u16, u16), heading: u16, i: usize) -> (u16, u16) {
    let a = heading as f32 / 65536.0 * std::f32::consts::TAU;
    let (right, back) = ((i % 2) as f32 * 384.0, (i / 2) as f32 * 640.0);
    (
        (spawn.0 as f32 + a.cos() * right - a.sin() * back) as u16,
        (spawn.1 as f32 + a.sin() * right + a.cos() * back) as u16,
    )
}

/// The map: what MAP.PCX + COL.PCX + the optional CHECK.PCX describe beyond the terrain itself.
pub struct Level {
    pub track: Option<Track>,
    /// Identifies these exact files, so best laps belong to one track; None without a time trial.
    pub key: Option<u64>,
    /// Hash of MAP + COL (+ CHECK, OBJECTS): replays only replay on the files they were recorded on.
    pub hash: u64,
    /// (x, y, gate) for every painted gate cell.
    /// (x, y, zone) for every painted zone cell.
    pub zone_cells: Vec<(usize, usize, Zone)>,
    /// MAPTEX tile sheet for this map's palette: the map folder's own, or the shared one when
    /// the palettes match (TEREP1's BMP palette doesn't, so it draws flat colours).
    pub maptex: Option<Vec<u8>>,
    /// Trees and the like from the optional OBJECTS.PCX (#47), and the sprite sheet they're drawn
    /// from: the folder's own OBJTEX.PCX, else the shared one.
    pub objects: Vec<objects::Object>,
    pub objtex: Option<pcx::Image>,
    /// Each object kind's trunk (OBJTEX.TXT, else measured off the sprite).
    pub colliders: objects::Colliders,
    /// From the folder's optional INFO.TXT: the map's name (first line) and its credits (the rest).
    pub title: Option<String>,
    pub credits: String,
}

pub fn load_level(name: &str) -> Result<(Terrain, Level), String> {
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
    pub fn trunks(&self) -> Vec<Trunk> {
        self.objects
            .iter()
            .filter_map(|o| o.trunk(&self.colliders))
            .collect()
    }
}

/// FNV-1a over the map files: best laps belong to this exact track.
pub fn track_hash(files: &[&[u8]]) -> u64 {
    files
        .iter()
        .flat_map(|f| f.iter())
        .fold(0xcbf29ce484222325, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        })
}
