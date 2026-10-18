//! Optional baked terrain lighting ("Lighting: baked"): soft sun shadows, sky occlusion and one
//! bounce of coloured light, computed per terrain vertex when a map loads (~25 ms natively).
//! The sun's angle to each slope (N·L) is left to the mesher, so ground dug afterwards still
//! catches the sun right; only its shadow and occlusion keep their load-time values.
//!
//! Distances are in cells, heights in cells too (height index / 16, as the world's 16 units per
//! step over 256 per cell).

use crate::terrain::{SIZE, Terrain};

/// Towards the sun: the azimuth of the game's flat shading, 35° up so hills cast real shadows.
const SUN_AZIMUTH: [f32; 2] = [-0.8, 0.6];
const SUN_ELEVATION: f32 = 0.61; // radians, 35°
/// Warm sun, cool sky (vertex colours, so ≤ 1 after adding up).
const SUN: [f32; 3] = [0.68, 0.64, 0.56];
const SKY: [f32; 3] = [0.5, 0.55, 0.62];
const BOUNCE: f32 = 0.9;

/// The load-time part of the lighting, one entry per terrain vertex (row-major, `SIZE²`).
pub struct Baked {
    /// 0 (in shadow) – 1 (in the sun), soft at the edges.
    pub sun: Vec<f32>,
    /// Share of the sky the vertex sees, 0–1.
    pub sky: Vec<f32>,
    /// Light bounced off the ground around it, RGB.
    pub bounce: Vec<[f32; 3]>,
}

/// The unit vector towards the sun in render space (x, up, y), for N·L.
pub fn sun_direction() -> [f32; 3] {
    let (s, c) = SUN_ELEVATION.sin_cos();
    [SUN_AZIMUTH[0] * c, s, SUN_AZIMUTH[1] * c]
}

/// Height in cells at a cell position, bilinear; None off the map (open sky).
fn height(t: &Terrain, x: f32, y: f32) -> Option<f32> {
    if !(0.0..=(SIZE - 1) as f32).contains(&x) || !(0.0..=(SIZE - 1) as f32).contains(&y) {
        return None;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(SIZE - 1), (y0 + 1).min(SIZE - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let g = |x: usize, y: usize| t.height[y * SIZE + x] as f32 / 16.0;
    let a = g(x0, y0) + fx * (g(x1, y0) - g(x0, y0));
    let b = g(x0, y1) + fx * (g(x1, y1) - g(x0, y1));
    Some(a + fy * (b - a))
}

/// N·L of vertex (x, y) for the current heights, with the mesher's normal.
pub fn sun_angle(t: &Terrain, x: usize, y: usize) -> f32 {
    let g = |x: usize, y: usize| t.height[y.min(SIZE - 1) * SIZE + x.min(SIZE - 1)] as f32 / 16.0;
    let n = [
        g(x.saturating_sub(1), y) - g(x + 1, y),
        2.0,
        g(x, y.saturating_sub(1)) - g(x, y + 1),
    ];
    let l = sun_direction();
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    ((n[0] * l[0] + n[1] * l[1] + n[2] * l[2]) / len).max(0.0)
}

pub fn bake(t: &Terrain) -> Baked {
    let n = SIZE * SIZE;
    let tan = SUN_ELEVATION.tan();
    let top = *t.height.iter().max().unwrap_or(&0) as f32 / 16.0;
    // soft shadow: march towards the sun; the penumbra is the tightest clearance / distance
    // (Quílez, "soft shadows in raymarched SDFs"), done once the ray clears the highest point
    let sun: Vec<f32> = (0..n)
        .map(|i| {
            let (x, y) = ((i % SIZE) as f32, (i / SIZE) as f32);
            let h0 = t.height[i] as f32 / 16.0 + 0.05;
            let (mut v, mut d) = (1f32, 0.5f32);
            while h0 + d * tan <= top {
                match height(t, x + SUN_AZIMUTH[0] * d, y + SUN_AZIMUTH[1] * d) {
                    None => break,
                    Some(g) => v = v.min(4.0 * (h0 + d * tan - g) / d),
                }
                d += 0.5;
            }
            v.clamp(0.0, 1.0)
        })
        .collect();
    // sky: the highest horizon in 12 directions out to 40 cells (horizon-based occlusion)
    let dirs: Vec<(f32, f32)> = (0..12)
        .map(|k| (k as f32 * std::f32::consts::TAU / 12.0).sin_cos())
        .collect();
    let reach = [1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 18.0, 27.0, 40.0f32];
    let sky: Vec<f32> = (0..n)
        .map(|i| {
            let (x, y) = ((i % SIZE) as f32, (i / SIZE) as f32);
            let h0 = t.height[i] as f32 / 16.0;
            let blocked: f32 = dirs
                .iter()
                .map(|&(dx, dy)| {
                    let slope = reach
                        .iter()
                        .filter_map(|&d| Some((height(t, x + dx * d, y + dy * d)? - h0) / d))
                        .fold(0f32, f32::max);
                    slope / (1.0 + slope * slope).sqrt() // sin of the horizon angle
                })
                .sum();
            1.0 - blocked / dirs.len() as f32
        })
        .collect();
    // one bounce: what each vertex sends out (its colour × its light), blurred over ~8 cells
    let mut bounce: Vec<[f32; 3]> = (0..n)
        .map(|i| {
            let p = t.palette[t.colour[i] as usize];
            let e = sun[i] * sun_angle(t, i % SIZE, i / SIZE) + 0.5 * sky[i];
            p.map(|c| c as f32 / 255.0 * e)
        })
        .collect();
    for _ in 0..2 {
        for (dx, dy) in [(1, 0), (0, 1)] {
            let src = bounce.clone();
            for (i, out) in bounce.iter_mut().enumerate() {
                let (x, y) = ((i % SIZE) as i32, (i / SIZE) as i32);
                let (mut sum, mut count) = ([0f32; 3], 0.0);
                for k in -4..=4 {
                    let (xx, yy) = (x + dx * k, y + dy * k);
                    if (0..SIZE as i32).contains(&xx) && (0..SIZE as i32).contains(&yy) {
                        let s = src[yy as usize * SIZE + xx as usize];
                        (0..3).for_each(|c| sum[c] += s[c]);
                        count += 1.0;
                    }
                }
                *out = sum.map(|s| s / count);
            }
        }
    }
    Baked { sun, sky, bounce }
}

impl Baked {
    /// The light on vertex (x, y), RGB 0–1, for the slope's current N·L.
    pub fn light(&self, x: usize, y: usize, sun_angle: f32) -> [f32; 3] {
        let i = y * SIZE + x;
        let (sun, sky) = (self.sun[i] * sun_angle, self.sky[i]);
        [0, 1, 2].map(|c| {
            // the bounce fills in where the sky is hidden: valleys, hollows
            (SUN[c] * sun + SKY[c] * sky + BOUNCE * self.bounce[i][c] * (1.0 - sky)).min(1.0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terrain(h: impl Fn(usize, usize) -> u8) -> Terrain {
        let mut palette = [[0u8; 3]; 256];
        palette[1] = [200, 200, 200];
        Terrain {
            height: (0..SIZE * SIZE).map(|i| h(i % SIZE, i / SIZE)).collect(),
            colour: vec![1; SIZE * SIZE],
            palette,
        }
    }

    #[test]
    fn flat_ground_is_lit_evenly_and_like_the_flat_shading() {
        let t = terrain(|_, _| 40);
        let b = bake(&t);
        let l = b.light(128, 128, sun_angle(&t, 128, 128));
        assert_eq!(b.light(10, 200, sun_angle(&t, 10, 200)), l);
        assert!((b.sun[128 * SIZE + 128] - 1.0).abs() < 1e-6);
        // open ground about as bright as the game's flat shading (0.93 on flat ground)
        let grey = (l[0] + l[1] + l[2]) / 3.0;
        assert!((0.85..=1.0).contains(&grey), "{l:?}");
    }

    #[test]
    fn a_wall_shades_the_ground_behind_it() {
        // a wall across the map at x = 120..124; the sun is towards −x, so x > 124 is behind it
        let t = terrain(|x, _| if (120..124).contains(&x) { 200 } else { 10 });
        let b = bake(&t);
        let (behind, front) = (b.sun[128 * SIZE + 126], b.sun[128 * SIZE + 110]);
        assert!(behind < 0.1, "behind the wall: {behind}");
        assert!(front > 0.99, "in front: {front}");
    }

    #[test]
    fn a_pit_sees_less_sky() {
        let t = terrain(|x, y| {
            if x.abs_diff(128) < 3 && y.abs_diff(128) < 3 {
                0
            } else {
                60
            }
        });
        let b = bake(&t);
        assert!(b.sky[128 * SIZE + 128] < 0.6 * b.sky[30 * SIZE + 30]);
        assert!(b.sky[30 * SIZE + 30] > 0.99);
    }
}
