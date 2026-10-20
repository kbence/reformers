//! The map edits themselves, on plain grids so they're tested without a window: brushes,
//! smoothing and the ramp on the heights; painting on the colours; the checkpoint layer and
//! its gate order.

use reformers::pcx;
use reformers::terrain::SIZE;
use reformers::track::{GATE, SPAWN};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shape {
    Circle,
    Rect,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edge {
    Hard,
    /// Anti-aliased: edge cells get the share of them the shape covers.
    Smooth,
    /// Fades out over `feather` cells.
    Feather,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Brush {
    pub shape: Shape,
    /// Across, in cells.
    pub size: u32,
    pub edge: Edge,
    pub feather: f32,
}

impl Brush {
    pub const PENCIL: Brush = Brush {
        shape: Shape::Rect,
        size: 1,
        edge: Edge::Hard,
        feather: 0.0,
    };

    /// Where the brush centres for the mouse at cell position `m` (fractional cells): on a cell
    /// centre for odd sizes, on a cell corner for even ones, so an N brush covers N cells.
    pub fn centre(&self, m: [f32; 2]) -> [f32; 2] {
        if self.size % 2 == 1 {
            m.map(|v| v.floor() + 0.5)
        } else {
            m.map(|v| v.round())
        }
    }

    fn weight(&self, dx: f32, dy: f32) -> f32 {
        let r = self.size as f32 / 2.0;
        let d = match self.shape {
            Shape::Circle => dx.hypot(dy),
            Shape::Rect => dx.abs().max(dy.abs()),
        };
        match self.edge {
            Edge::Hard => (d < r) as u8 as f32,
            Edge::Smooth => (r + 0.5 - d).clamp(0.0, 1.0),
            Edge::Feather => {
                let f = self.feather.max(0.5);
                let t = ((r - d) / f).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
        }
    }

    /// The cells under the brush centred at `c` (from `centre`), with weights 0–1.
    // ponytail: clipped at the map edge, not wrapped; the game keeps cars well inside it
    pub fn cells(&self, c: [f32; 2]) -> Vec<(usize, f32)> {
        let r = self.size as f32 / 2.0 + 1.0;
        let range = |v: f32| (v - r).floor().max(0.0) as usize..((v + r).ceil() as usize).min(SIZE);
        let mut out = vec![];
        for y in range(c[1]) {
            for x in range(c[0]) {
                let w = self.weight(x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
                if w > 0.0 {
                    out.push((y * SIZE + x, w));
                }
            }
        }
        out
    }
}

/// Brush centres along a mouse move from `a` to `b` (cells), half a cell apart, so a fast
/// stroke leaves no gaps. `a` itself was stamped last frame.
pub fn stroke(a: [f32; 2], b: [f32; 2]) -> Vec<[f32; 2]> {
    let n = ((b[0] - a[0]).hypot(b[1] - a[1]) * 2.0).ceil().max(1.0) as usize;
    (1..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
        })
        .collect()
}

/// One stamp of "set": cells move from where they were when the stroke began (`base`) toward
/// `target` by the strongest weight this stroke gave them (`reach`), so going over a spot
/// again doesn't pile up.
pub fn set(h: &mut [f32], base: &[f32], reach: &mut [f32], cells: &[(usize, f32)], target: f32) {
    for &(i, w) in cells {
        reach[i] = reach[i].max(w);
        h[i] = base[i] + (target - base[i]) * reach[i];
    }
}

/// One stamp of "raise" (negative `amount` lowers).
pub fn raise(h: &mut [f32], cells: &[(usize, f32)], amount: f32) {
    for &(i, w) in cells {
        h[i] = (h[i] + amount * w).clamp(0.0, 255.0);
    }
}

fn blurred(h: &[f32], i: usize) -> f32 {
    let (x, y) = (i % SIZE, i / SIZE);
    let (mut sum, mut n) = (0.0, 0.0);
    for ny in y.saturating_sub(1)..(y + 2).min(SIZE) {
        for nx in x.saturating_sub(1)..(x + 2).min(SIZE) {
            sum += h[ny * SIZE + nx];
            n += 1.0;
        }
    }
    sum / n
}

/// One stamp of smoothing: each cell moves toward its 3×3 average by weight × `k` (0–1).
pub fn smooth(h: &mut [f32], cells: &[(usize, f32)], k: f32) {
    let new: Vec<f32> = cells
        .iter()
        .map(|&(i, w)| h[i] + (blurred(h, i) - h[i]) * (w * k).min(1.0))
        .collect();
    for (&(i, _), v) in cells.iter().zip(new) {
        h[i] = v;
    }
}

/// `passes` 3×3 box blurs over the whole map.
pub fn smooth_all(h: &mut Vec<f32>, passes: u32) {
    for _ in 0..passes {
        *h = (0..h.len()).map(|i| blurred(h, i)).collect();
    }
}

/// The cells a ramp from cell `a` to cell `b`, `width` cells wide, covers, with how far along
/// it each one is (0 at `a`, 1 at `b`). Square ends, hard sides.
pub fn ramp_cells(a: (usize, usize), b: (usize, usize), width: u32) -> Vec<(usize, f32)> {
    let (ax, ay) = (a.0 as f32 + 0.5, a.1 as f32 + 0.5);
    let (dx, dy) = (b.0 as f32 - a.0 as f32, b.1 as f32 - a.1 as f32);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return vec![];
    }
    let half = width as f32 / 2.0;
    let len = len2.sqrt();
    let mut out = vec![];
    for i in 0..SIZE * SIZE {
        let (px, py) = ((i % SIZE) as f32 + 0.5 - ax, (i / SIZE) as f32 + 0.5 - ay);
        let t = (px * dx + py * dy) / len2;
        let side = (px * dy - py * dx) / len;
        // ponytail: scans the whole map (65k cells) once per ramp; a bounding box if it ever lags
        if (-0.5 / len..=1.0 + 0.5 / len).contains(&t) && side.abs() < half {
            out.push((i, t.clamp(0.0, 1.0)));
        }
    }
    out
}

/// A ramp: a straight slope from the height at `a` to `target` at `b`.
pub fn ramp(h: &mut [f32], a: (usize, usize), b: (usize, usize), width: u32, target: f32) {
    let start = h[a.1 * SIZE + a.0];
    for (i, t) in ramp_cells(a, b, width) {
        h[i] = start + (target - start) * t;
    }
}

/// Hard-edged paint of `v` (the colour layer, the checkpoint zones, the eraser).
pub fn paint(grid: &mut [u8], cells: &[(usize, f32)], v: u8) {
    for &(i, w) in cells {
        if w >= 0.5 {
            grid[i] = v;
        }
    }
}

/// A rectangle of a 256×256 sheet painted as one picture: a MAPTEX tile (16×16; strokes wrap
/// at its edges so a pattern lines up from cell to cell, as the game repeats a tile once per
/// cell) or an OBJTEX sprite slot (32×64, no wrap).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cel {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub wrap: bool,
}

impl Cel {
    /// MAPTEX tile `t`: the texture of COL colour `t` (row = material, column = shade).
    pub fn tile(t: u8) -> Cel {
        let (x, y) = ((t & 15) as usize * 16, (t >> 4) as usize * 16);
        Cel {
            x,
            y,
            w: 16,
            h: 16,
            wrap: true,
        }
    }

    /// OBJTEX slot of object kind `kind` (1–15; see `reformers::objects`).
    pub fn sprite(kind: u8) -> Cel {
        let s = kind.max(1) as usize - 1;
        let [w, h] = reformers::objects::SLOT;
        Cel {
            x: s % 8 * w,
            y: s / 8 * h,
            w,
            h,
            wrap: false,
        }
    }

    /// The sheet index of pixel (x, y) in the cel (wrapping).
    pub fn px(&self, x: usize, y: usize) -> usize {
        (self.y + y % self.h) * SIZE + self.x + x % self.w
    }

    fn inside(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        if self.wrap {
            Some((
                x.rem_euclid(self.w as i32) as usize,
                y.rem_euclid(self.h as i32) as usize,
            ))
        } else {
            ((0..self.w as i32).contains(&x) && (0..self.h as i32).contains(&y))
                .then_some((x as usize, y as usize))
        }
    }

    /// A round dab of `size` pixels at (x, y).
    pub fn dab(&self, sheet: &mut [u8], x: i32, y: i32, size: i32, v: u8) {
        let r = size as f32 / 2.0;
        let (lo, hi) = (-(size / 2), (size - 1) / 2);
        let off = if size % 2 == 0 { 0.5 } else { 0.0 };
        for dy in lo..=hi {
            for dx in lo..=hi {
                let round =
                    ((dx as f32 + off).powi(2) + (dy as f32 + off).powi(2)).sqrt() < r + 0.01;
                if (round || size <= 2)
                    && let Some((px, py)) = self.inside(x + dx, y + dy)
                {
                    sheet[self.px(px, py)] = v;
                }
            }
        }
    }

    /// Flood fill from (x, y): the 4-connected pixels of that colour become `v`.
    pub fn fill(&self, sheet: &mut [u8], x: usize, y: usize, v: u8) {
        let old = sheet[self.px(x, y)];
        if old == v {
            return;
        }
        let mut todo = vec![(x, y)];
        while let Some((x, y)) = todo.pop() {
            let i = self.px(x, y);
            if sheet[i] != old {
                continue;
            }
            sheet[i] = v;
            if x > 0 {
                todo.push((x - 1, y));
            }
            if x + 1 < self.w {
                todo.push((x + 1, y));
            }
            if y > 0 {
                todo.push((x, y - 1));
            }
            if y + 1 < self.h {
                todo.push((x, y + 1));
            }
        }
    }

    pub fn copy(&self, sheet: &[u8]) -> Vec<u8> {
        (0..self.w * self.h)
            .map(|i| sheet[self.px(i % self.w, i / self.w)])
            .collect()
    }

    /// `pixels` from `copy` of a cel the same size (else nothing happens).
    pub fn paste(&self, sheet: &mut [u8], pixels: &[u8]) {
        if pixels.len() == self.w * self.h {
            for (i, &v) in pixels.iter().enumerate() {
                sheet[self.px(i % self.w, i / self.w)] = v;
            }
        }
    }
}

/// A sheet of flat tiles, each its own colour: what the game draws for maps without a MAPTEX.
pub fn flat_sheet() -> Vec<u8> {
    (0..SIZE * SIZE)
        .map(|i| ((i / SIZE / 16) * 16 + i % SIZE / 16) as u8)
        .collect()
}

/// xorshift32: the editor's random numbers (noise, scattering). Never 0 in, never 0 out.
pub fn rng(r: &mut u32) -> u32 {
    *r ^= *r << 13;
    *r ^= *r >> 17;
    *r ^= *r << 5;
    *r
}

/// Tile `t` as noise around colour `base`: each pixel a shade of `base`'s material row up to
/// `amount` shades either side, like the original sheet's swatches. `seed` varies the pattern.
pub fn tile_noise(sheet: &mut [u8], t: u8, base: u8, amount: u8, seed: u32) {
    let mut r = seed.wrapping_mul(2654435761) | 1;
    let cel = Cel::tile(t);
    for y in 0..16 {
        for x in 0..16 {
            let spread = 2 * amount as i32 + 1;
            let shade = ((base & 15) as i32 + (rng(&mut r) % spread as u32) as i32 - amount as i32)
                .clamp(0, 15);
            sheet[cel.px(x, y)] = (base & 0xF0) | shade as u8;
        }
    }
}

/// One stamp of the forest brush on OBJECTS: each cell under it not `seen` yet this stroke gets,
/// with chance `density` (0–1), an object of a random kind from `kinds` and size in `sizes`.
/// Cells already holding one keep it.
pub fn scatter(
    grid: &mut [u8],
    cells: &[(usize, f32)],
    seen: &mut [bool],
    kinds: &[u8],
    sizes: (u8, u8),
    density: f32,
    r: &mut u32,
) {
    if kinds.is_empty() {
        return;
    }
    let (lo, hi) = (sizes.0.min(sizes.1), sizes.0.max(sizes.1));
    for &(i, w) in cells {
        if w < 0.5 || std::mem::replace(&mut seen[i], true) || grid[i] != 0 {
            continue;
        }
        if (rng(r) % 1000) as f32 >= density * 1000.0 {
            continue;
        }
        let kind = kinds[rng(r) as usize % kinds.len()];
        let size = lo + (rng(r) % (hi - lo + 1) as u32) as u8;
        grid[i] = kind << 4 | size;
    }
}

/// The checkpoint layer (CHECK.PCX, format in `track.rs`): one index per terrain cell.
#[derive(Clone, PartialEq, Debug)]
pub struct Layer(pub Vec<u8>);

impl Layer {
    fn set(&mut self, x: i32, y: i32, v: u8) {
        if (0..SIZE as i32).contains(&x) && (0..SIZE as i32).contains(&y) {
            self.0[y as usize * SIZE + x as usize] = v;
        }
    }

    /// The one spawn: a 3×3 blob at (x, y) facing `dir` (0–3: up, right, down, left on the map).
    pub fn set_spawn(&mut self, x: i32, y: i32, dir: u8) {
        self.0
            .iter_mut()
            .filter(|v| **v >> 4 == SPAWN)
            .for_each(|v| *v = 0);
        for (dx, dy) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))) {
            self.set(x + dx, y + dy, SPAWN << 4 | dir);
        }
    }

    /// The spawn's centre cell and direction, if there is one.
    pub fn spawn(&self) -> Option<(f32, f32, u8)> {
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
    pub fn gate_line(&mut self, a: (i32, i32), b: (i32, i32), n: u8) {
        let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).max(1);
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let x = a.0 as f32 + (b.0 - a.0) as f32 * t;
            let y = a.1 as f32 + (b.1 - a.1) as f32 * t;
            self.set(x.round() as i32, y.round() as i32, GATE << 4 | n);
        }
    }

    /// The gate settings in use, ascending (= lap order; the first is start/finish).
    pub fn gates(&self) -> Vec<u8> {
        let mut used = [false; 16];
        self.0
            .iter()
            .filter(|v| **v >> 4 == GATE)
            .for_each(|v| used[(v & 15) as usize] = true);
        (0..16).filter(|&n| used[n as usize]).collect()
    }

    /// The setting a new gate gets: after the last one, or the first gap when 15 is taken.
    pub fn next_gate(&self) -> Option<u8> {
        let used = self.gates();
        match used.last() {
            None => Some(0),
            Some(&last) if last < 15 => Some(last + 1),
            _ => (0..16).find(|n| !used.contains(n)),
        }
    }

    pub fn erase_gate(&mut self, n: u8) {
        self.0
            .iter_mut()
            .filter(|v| **v == GATE << 4 | n)
            .for_each(|v| *v = 0);
    }

    /// Renames gate settings through `f` (old setting → new).
    fn remap(&mut self, f: impl Fn(u8) -> u8) {
        for v in self.0.iter_mut().filter(|v| **v >> 4 == GATE) {
            *v = GATE << 4 | f(*v & 15);
        }
    }

    /// Settings 0, 1, 2… in lap order, no gaps.
    fn compact(&mut self) {
        let used = self.gates();
        self.remap(|s| used.iter().position(|&u| u == s).unwrap() as u8);
    }

    /// Swaps gate `rank` (lap order) with the one before (`up`) or after it.
    pub fn move_gate(&mut self, rank: usize, up: bool) {
        let used = self.gates();
        let other = if up {
            rank.checked_sub(1)
        } else {
            Some(rank + 1)
        };
        if let Some(o) = other.filter(|&o| o < used.len())
            && rank < used.len()
        {
            let (a, b) = (used[rank], used[o]);
            self.remap(|s| match s {
                _ if s == a => b,
                _ if s == b => a,
                _ => s,
            });
        }
    }

    /// Makes room for a gate right after the one with setting `s`: compacts, shifts the later
    /// gates up, and returns the free setting. None when all 16 are taken.
    pub fn insert_after(&mut self, s: u8) -> Option<u8> {
        let rank = self.gates().iter().position(|&u| u == s)? as u8;
        if self.gates().len() >= 16 {
            return None;
        }
        self.compact();
        self.remap(|v| if v > rank { v + 1 } else { v });
        Some(rank + 1)
    }

    /// Every gate piece (a gate on a fork has one per branch): (setting, cells).
    pub fn pieces(&self) -> Vec<(u8, Vec<(usize, usize)>)> {
        self.gates()
            .into_iter()
            .flat_map(|s| {
                reformers::track::pieces(&self.0, GATE << 4 | s)
                    .into_iter()
                    .map(move |p| (s, p))
            })
            .collect()
    }

    /// Renumbers the gates from pieces clicked in driving order: `order[k]` = the rank piece
    /// `k` (an index into `pieces()`) gets, and pieces sharing a rank form one gate (a fork).
    /// Pieces not clicked keep their order after the clicked ones. Err when that's > 16 gates.
    pub fn renumber(&mut self, order: &[(usize, u8)]) -> Result<(), String> {
        let pieces = self.pieces();
        // sort key per piece: clicked by rank first, then the rest by their old setting
        let key = |k: usize| match order.iter().find(|o| o.0 == k) {
            Some(&(_, rank)) => (0, rank),
            None => (1, pieces[k].0),
        };
        let mut keys: Vec<_> = (0..pieces.len()).map(key).collect();
        keys.sort();
        keys.dedup();
        if keys.len() > 16 {
            return Err(format!("{} gates; CHECK.PCX holds 16", keys.len()));
        }
        for (k, (_, cells)) in pieces.iter().enumerate() {
            let s = keys.iter().position(|&x| x == key(k)).unwrap() as u8;
            for &(x, y) in cells {
                self.0[y * SIZE + x] = GATE << 4 | s;
            }
        }
        Ok(())
    }

    /// As CHECK.PCX: the 320-wide layout of the originals (blank legend strip), colours per
    /// element so it reads in a paint program too.
    pub fn encode(&self) -> Vec<u8> {
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
                let [r, g, b, _] = colour(i as u8);
                [r, g, b].map(|v| (v * 255.0) as u8)
            }),
        })
    }
}

/// How a checkpoint element looks (also the saved palette), RGBA; brighter for stronger zones.
pub fn colour(v: u8) -> [f32; 4] {
    use reformers::track::{BOOST, RESET, SLOW, SOFT};
    let shade = 0.55 + 0.45 * (v & 15) as f32 / 15.0;
    let [r, g, b] = match v >> 4 {
        SPAWN => [1.0, 1.0, 1.0],
        GATE => [1.0, 0.85, 0.1],
        RESET => [0.9, 0.1, 0.1],
        BOOST => [0.2, 0.9, 1.0],
        SLOW => [0.55, 0.35, 0.12],
        SOFT => [0.95, 0.55, 0.15],
        _ => return [0.0; 4],
    };
    let s = if v >> 4 == SPAWN || v >> 4 == GATE {
        1.0
    } else {
        shade
    };
    [r * s, g * s, b * s, 0.85]
}

#[cfg(test)]
mod tests {
    use super::*;
    use reformers::track::{BOOST, RESET, SOFT, Track, Zone};

    fn at(x: usize, y: usize) -> usize {
        y * SIZE + x
    }

    #[test]
    fn cels() {
        let mut sheet = flat_sheet();
        let t = Cel::tile(0x35);
        assert_eq!(sheet[t.px(3, 7)], 0x35, "flat tiles are their own colour");
        assert_eq!(t.px(0, 0), 48 * SIZE + 80);
        // a tile dab at the corner wraps to the opposite edges and stays in the tile
        t.dab(&mut sheet, 0, 0, 3, 9);
        for (x, y) in [(0, 0), (15, 0), (0, 15), (15, 15), (1, 0), (0, 1)] {
            assert_eq!(sheet[t.px(x, y)], 9, "({x}, {y})");
        }
        assert_eq!(
            sheet[Cel::tile(0x34).px(15, 0)],
            0x34,
            "the neighbour is untouched"
        );
        assert_eq!(t.copy(&sheet).iter().filter(|&&v| v == 9).count(), 9);
        t.fill(&mut sheet, 8, 8, 7);
        assert_eq!(t.copy(&sheet).iter().filter(|&&v| v == 7).count(), 256 - 9);
        // noise stays in the base colour's material row, within the amount
        tile_noise(&mut sheet, 0x35, 0x48, 2, 1);
        let tile = t.copy(&sheet);
        assert!(
            tile.iter()
                .all(|&v| v >> 4 == 4 && (6..=10).contains(&(v & 15)))
        );
        assert!(tile.iter().any(|&v| v != tile[0]), "it varies");
        Cel::tile(0).paste(&mut sheet, &tile);
        assert_eq!(Cel::tile(0).copy(&sheet), tile);
        // a sprite slot doesn't wrap: a dab at its corner stays in the corner
        let mut sheet = vec![0u8; SIZE * SIZE];
        let s = Cel::sprite(10); // slot 9: (32, 64)
        assert_eq!(s.px(0, 0), 64 * SIZE + 32);
        s.dab(&mut sheet, 0, 0, 3, 5);
        assert_eq!(
            sheet.iter().filter(|&&v| v == 5).count(),
            4,
            "the quarter inside"
        );
        s.fill(&mut sheet, 10, 10, 6);
        assert_eq!(sheet.iter().filter(|&&v| v == 6).count(), 32 * 64 - 4);
        let tile = Cel::tile(0).copy(&sheet);
        Cel::sprite(1).paste(&mut sheet, &tile); // wrong size: ignored
        assert_eq!(sheet[0], 0);
    }

    #[test]
    fn scatters_forests() {
        let mut grid = vec![0u8; SIZE * SIZE];
        grid[at(10, 10)] = 0x61; // a rock already there
        let b = Brush {
            shape: Shape::Rect,
            size: 9,
            edge: Edge::Hard,
            feather: 0.0,
        };
        let cells = b.cells(b.centre([10.5, 10.5]));
        let (mut seen, mut r) = (vec![false; SIZE * SIZE], 7u32);
        scatter(&mut grid, &cells, &mut seen, &[1, 2], (3, 5), 1.0, &mut r);
        assert_eq!(grid[at(10, 10)], 0x61, "kept");
        let placed: Vec<u8> = cells
            .iter()
            .map(|c| grid[c.0])
            .filter(|&v| v != 0x61)
            .collect();
        assert_eq!(placed.len(), 80, "density 1: every free cell");
        assert!(
            placed
                .iter()
                .all(|&v| matches!(v >> 4, 1 | 2) && (3..=5).contains(&(v & 15)))
        );
        assert!(placed.iter().any(|&v| v >> 4 == 1) && placed.iter().any(|&v| v >> 4 == 2));
        // half density over fresh ground: about half; the same stroke again adds nothing
        let mut grid = vec![0u8; SIZE * SIZE];
        let mut seen = vec![false; SIZE * SIZE];
        scatter(&mut grid, &cells, &mut seen, &[3], (0, 0), 0.5, &mut r);
        let n = grid.iter().filter(|&&v| v != 0).count();
        assert!((25..=56).contains(&n), "{n}");
        scatter(&mut grid, &cells, &mut seen, &[3], (0, 0), 0.5, &mut r);
        assert_eq!(grid.iter().filter(|&&v| v != 0).count(), n);
    }

    #[test]
    fn brush_sizes_cover_that_many_cells() {
        for size in [1, 2, 3, 4, 7] {
            let b = Brush {
                shape: Shape::Rect,
                size,
                edge: Edge::Hard,
                feather: 0.0,
            };
            let n = b.cells(b.centre([100.3, 100.7])).len();
            assert_eq!(n, (size * size) as usize, "size {size}");
        }
        let round = Brush {
            shape: Shape::Circle,
            size: 9,
            edge: Edge::Hard,
            feather: 0.0,
        };
        let n = round.cells(round.centre([50.0, 50.0])).len() as f32;
        assert!((n - 63.6).abs() < 6.0, "a disc of radius 4.5: {n}");
        // smooth edges: partial weights, never more than hard plus a ring
        let aa = Brush {
            edge: Edge::Smooth,
            ..round
        };
        let w = aa.cells(aa.centre([50.0, 50.0]));
        assert!(w.iter().any(|c| c.1 > 0.0 && c.1 < 1.0));
        // feathered: the middle is full, it fades out
        let f = Brush {
            edge: Edge::Feather,
            feather: 3.0,
            ..round
        };
        let w = f.cells(f.centre([50.0, 50.0]));
        assert_eq!(w.iter().find(|c| c.0 == at(50, 50)).unwrap().1, 1.0);
        let edge = w.iter().find(|c| c.0 == at(54, 50)).unwrap().1; // 4 of 4.5 out, 3-cell fade
        assert!(edge > 0.0 && edge < 0.5, "{edge}");
    }

    #[test]
    fn set_doesnt_pile_up_and_raise_does() {
        let b = Brush {
            shape: Shape::Circle,
            size: 5,
            edge: Edge::Feather,
            feather: 2.0,
        };
        let base = vec![10.0; SIZE * SIZE];
        let (mut h, mut reach) = (base.clone(), vec![0.0; SIZE * SIZE]);
        let cells = b.cells(b.centre([20.0, 20.0]));
        set(&mut h, &base, &mut reach, &cells, 100.0);
        let once = h.clone();
        set(&mut h, &base, &mut reach, &cells, 100.0);
        assert_eq!(h, once, "the same stroke over the same spot");
        assert_eq!(h[at(20, 20)], 100.0);
        let mut r = base.clone();
        raise(&mut r, &cells, 5.0);
        raise(&mut r, &cells, 5.0);
        assert_eq!(r[at(20, 20)], 20.0);
        raise(&mut r, &cells, -1000.0);
        assert_eq!(r[at(20, 20)], 0.0, "clamped");
    }

    #[test]
    fn smoothing_flattens_a_spike() {
        let mut h = vec![0.0; SIZE * SIZE];
        h[at(30, 30)] = 90.0;
        let b = Brush {
            shape: Shape::Rect,
            size: 5,
            edge: Edge::Hard,
            feather: 0.0,
        };
        smooth(&mut h, &b.cells(b.centre([30.5, 30.5])), 1.0);
        assert_eq!(h[at(30, 30)], 10.0);
        assert_eq!(h[at(31, 31)], 10.0);
        let sum: f32 = h.iter().sum();
        let mut all = h.clone();
        smooth_all(&mut all, 3);
        assert!(all[at(30, 30)] < 5.0);
        assert!((all.iter().sum::<f32>() - sum).abs() < 1.0, "volume stays");
    }

    #[test]
    fn ramp_slopes_from_start_to_target() {
        let mut h = vec![0.0; SIZE * SIZE];
        h[at(10, 50)] = 20.0;
        ramp(&mut h, (10, 50), (30, 50), 4, 120.0);
        assert_eq!(h[at(10, 50)], 20.0);
        assert_eq!(h[at(30, 50)], 120.0);
        assert_eq!(h[at(20, 50)], 70.0);
        assert_eq!(h[at(20, 51)], 70.0, "across the width it's level");
        assert_eq!(h[at(20, 53)], 0.0, "hard sides: 4 wide");
        assert_eq!(h[at(35, 50)], 0.0, "nothing past the end");
        let cells = ramp_cells((5, 5), (5, 5), 3);
        assert!(cells.is_empty(), "a click is not a ramp");
    }

    #[test]
    fn saves_what_the_game_reads() {
        let mut l = Layer(vec![0; SIZE * SIZE]);
        l.set_spawn(100, 120, 1);
        l.set_spawn(128, 140, 2); // a second spawn replaces the first
        assert_eq!(l.next_gate(), Some(0));
        l.gate_line((120, 130), (136, 130), 0);
        l.gate_line((200, 60), (200, 76), l.next_gate().unwrap());
        l.gate_line((40, 60), (56, 76), l.next_gate().unwrap());
        let b = |size| Brush {
            shape: Shape::Circle,
            size,
            edge: Edge::Hard,
            feather: 0.0,
        };
        let stamp = |l: &mut Layer, x: f32, y: f32, size, v| {
            let b = b(size);
            paint(&mut l.0, &b.cells(b.centre([x, y])), v)
        };
        stamp(&mut l, 60.5, 200.5, 5, BOOST << 4 | 9);
        stamp(&mut l, 90.5, 200.5, 3, RESET << 4);
        stamp(&mut l, 150.5, 220.5, 7, SOFT << 4);
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
        l.erase_gate(1);
        assert_eq!(l.gates(), [0, 2]);
        assert_eq!(l.next_gate(), Some(3));
        stamp(&mut l, 60.5, 200.5, 7, 0);
        assert_eq!(Track::load(&l.encode()).unwrap().zone_at(60, 200), None);
    }

    #[test]
    fn sixteen_gates_at_most() {
        let mut l = Layer(vec![0; SIZE * SIZE]);
        for n in 0..16 {
            l.gate_line((n * 10, 5), (n * 10, 15), l.next_gate().unwrap());
        }
        assert_eq!(l.next_gate(), None);
        assert_eq!(l.insert_after(3), None);
        l.erase_gate(4);
        assert_eq!(l.next_gate(), Some(4), "the gap");
    }

    /// Gate at column x, setting s: a short vertical line, so pieces are easy to tell apart.
    fn gates(list: &[(i32, u8)]) -> Layer {
        let mut l = Layer(vec![0; SIZE * SIZE]);
        for &(x, s) in list {
            l.gate_line((x, 10), (x, 14), s);
        }
        l
    }

    fn order(l: &Layer) -> Vec<(u8, usize)> {
        l.pieces().iter().map(|(s, c)| (*s, c[0].0)).collect()
    }

    #[test]
    fn reorders_gates() {
        let mut l = gates(&[(10, 0), (20, 4), (30, 9)]);
        l.move_gate(2, true);
        assert_eq!(order(&l), [(0, 10), (4, 30), (9, 20)]);
        l.move_gate(0, true); // nothing above the first
        l.move_gate(2, false); // nothing below the last
        assert_eq!(order(&l), [(0, 10), (4, 30), (9, 20)]);
        // insert after the start: compacted, the rest shift up
        assert_eq!(l.insert_after(0), Some(1));
        assert_eq!(order(&l), [(0, 10), (2, 30), (3, 20)]);
    }

    #[test]
    fn renumbers_by_clicks_with_forks() {
        // pieces in setting order: 10 (s0), 20 (s1), 30 (s2), 40 (s3)
        let mut l = gates(&[(10, 0), (20, 1), (30, 2), (40, 3)]);
        let p = |x: usize| l.pieces().iter().position(|q| q.1[0].0 == x).unwrap();
        // drive 40 → (20 and 30 are one fork) → 10
        let clicks = [(p(40), 0), (p(20), 1), (p(30), 1), (p(10), 2)];
        l.renumber(&clicks).unwrap();
        assert_eq!(order(&l), [(0, 40), (1, 20), (1, 30), (2, 10)]);
        // partial: only the first clicked; the rest follow in their old order
        let mut l = gates(&[(10, 0), (20, 1), (30, 2)]);
        let p30 = l.pieces().iter().position(|q| q.1[0].0 == 30).unwrap();
        l.renumber(&[(p30, 0)]).unwrap();
        assert_eq!(order(&l), [(0, 30), (1, 10), (2, 20)]);
        let t = Track::load(&l.encode()).unwrap();
        assert_eq!(t.gates.len(), 3);
    }
}
