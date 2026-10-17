# TEREP2 reverse-engineering notes

Source: `sonneveld_unpacked/TEREP2/TEREP2U.EXE` (unpacked `TEREP2.EXE`, 86 496 bytes).
Hand-written 16-bit real-mode x86 asm using 32-bit registers, unreal mode (`es:[edi]`) and
the FPU (only for `fsqrt`). Code segment `1000` (0x5cd0 bytes), data segment `15cd`
(DS = 0x5cd). Addresses below are `1000:xxxx` code offsets; `[0xNNNN]` are DS offsets.

All world quantities are **16.16 fixed point** unless noted. Agent-produced; verify
before relying on exact constants.

## Startup / files (`0x00dc`–`0x021c`, `0x24c0`–`0x256a`)

| File | Where | Notes |
|---|---|---|
| `sim.cfg` | `0x011b` | Two signed words read into `[0xe9e2]`, `[0xe9e4]`. Defaults `0x0800` (2048) and `0xf000` (−4096). Missing file → defaults. |
| `map.pcx` | `0x24c1` → segment `gs` | 256×256 height bytes (sidebar cropped by the loader: it stores only x < 256). |
| `col.pcx` | `0x24e1` → segment `fs` | Colour map. Palette read from end (−0x300) into `[0x1a4d]`. |
| `maptex.pcx` | `0x24d1` | Texture sheet. |
| `textures.pcx` | `0x255d` | Fallback skin. |
| `fog.dat` | `0x2528` | 0x1100 bytes → `[0x1d51]`. |
| `smoke.dat` | `0x2541` | 0x1000 bytes → `[0x2e51]`. |
| `carN.dat` / `carN.pcx` | `0x0193` loop | Loads cars until a file is missing. Car N starts at x = 0x80 − N·0x80 (cell units), y = 0x7a. |

## Terrain (`0x25c5`)

- Input: x in `ax`, y in `bx`, as 8.8 (high byte = cell, low byte = fraction). 1 cell = 256 world units.
- Height = `MAP[cy][cx] * 16`.
- Each cell is split into two triangles along the diagonal: (fx + fy ≤ ½) uses corners
  (0,0),(1,0),(0,1); otherwise (1,1),(0,1),(1,0). Planar interpolation via `0x256b` (2D cross product).

## Car file `CAR*.DAT` (loader `0x2454`)

The file is copied verbatim into a car struct (max 10 000 bytes, first 0x82 bytes zeroed first).

Header (words):

| Off | Meaning |
|---|---|
| 0x00 | offset of atom section (0x84 in all cars) |
| 0x02 | offset of spring section (from file start) |
| 0x04 | offset of display list (from file start) |
| 0x06 | 0x82 (header size) |
| 0x08 | drive mode: 0 = RWD (CAR3), 1 = FWD, 2 = 4WD (others) |
| 0x1e | runtime: segment of the car's skin PCX |
| 0x20 | runtime: offset of the first atom with type 0xffff (driver/camera) |

**Atoms** (count word, then 28-byte records; 29 or 31 per car):

| Off | Type | Meaning |
|---|---|---|
| 0x00 | 3 × i32 | position x, y, z (16.16). File holds car-local coords; loader adds the spawn position, z += terrain height + 100. x = width, y = length, z = up. |
| 0x0c | 3 × i32 | velocity (per step, 16.16), 0 in file |
| 0x18 | u16 | radius (wheels: 26 on atoms 0,1 = front/steered, 29 on atoms 2,3 = rear; 8 for the driver atom) |
| 0x1a | u16 | type: 0 = body, 2 = front (steered) wheel, 1 = rear wheel, 0xffff = driver/camera |

Atoms 0–3 are the wheels in every car.

**Springs** (count word, then 14-byte records; 125 in CAR1):

| Off | Meaning |
|---|---|
| 0x00 | atom a |
| 0x02 | atom b |
| 0x04 | rest length × 64 (i16; negative = "set from current length on first step") |
| 0x06 | type 1: original rest length × 64; types 0 and ≥2: previous length (damping) |
| 0x08 | low byte: type (bit 7 = broken). High byte: mass shift *s* |
| 0x0a | min length × 64 (rest/2 in files) |
| 0x0c | max length × 64 (rest·3/2 in files) |

**Display list** (from header 0x04 to end of file): renderer bytecode run by `0x1408`. One opcode byte,
then operands; jump table at `1000:0x1413`. Vertex operands index a projected-vertex buffer (10 bytes per entry
at DS `0x126`) that `0x1347` fills with **2 entries per atom**: 2i = the atom, 2i+1 = its shadow (atom x,y at
terrain height). A few extra entries after the atoms come from `0x11f0` (steered wheel axis).

Sizes verified: all five cars walk exactly to the end of the file (parser: `src/car.rs`).

| Op | Size (bytes) | Meaning / operands |
|---|---|---|
| 0, 6, 11–15 | 1 | end of (sub-)list / no-op |
| 1 | 5 | dot: vertex, colour |
| 2 | 7 | depth compare: vertex, vertex, word → flags |
| 3 | 7 | backface test on 3 vertices → flags (draws nothing) |
| 4 | 2 + 2(n+1) + 2 | flat polygon: n, n+1 vertex words (last = first; 0 for shadows), colour word (low byte = palette index) |
| 5 | 2 + 4(n+1) | shaded polygon: n, (vertex, shade) × (n+1) — unused by the shipped cars |
| 7 | 4 + 4(n+1) | shaded polygon: n, colour, (vertex, shade) × (n+1) — unused |
| 8 | 2 + 6(n+1) | textured polygon: n, (vertex word, u word, v word) × (n+1); u/v = skin pixel × 256 |
| 9 | 21 | textured sprite: vertex, size, 4 uv pairs — unused |
| 10 | 7 + 20·9 | wheel: centre vertex, axle vertex, 0, then 9 frames of (2 words, 4 uv pairs) chosen by view angle; frame 0 = face-on |
| 16 | 5 | draw two sub-lists (rel16, rel16) in an order set by the last test |
| 17 / 18 | 3 | jump / call rel16 |
| 19 / 20 | 3 | jump rel16 if the last test was negative / not negative |

Shadows are flat colour-0 polygons on odd entries. Front wheels' axle vertex is one of the two extra
entries (atom 0/1 ± steered side × 128); rear wheels point at each other. Skins: CAR1/CAR2.PCX ship; cars
without one use TEXTURES.PCX (`0x1323`). All colours go through COL.PCX's palette.

## Tick (`0x5680`, PIT at `[0xec50]` = 120 Hz)

The whole sim runs in the timer interrupt. Per tick, for each car: input `0x0d2a`, drive `0x1004`,
physics `0x48d0` (= one spring pass `0x4e0a`, then the atom pass `0x48db`). Then once: car–car
collision `0x500b`, particles `0x0bb5`, reset keys 1/2 `0x0a3b`. Startup patches the two relative calls
at `0x56ba`/`0x56c8` to point at `0x48d0`/`0x500b`. Rendering runs in the main loop, unsynchronised.

## Physics step

Per atom (`0x48db`–`0x497d`):
1. `vz -= [0x6a]` (gravity, 0x1800 = 0.09375 units/step²).
2. Ground contact if z < terrain height (`0x532e`) + radius → `0x4a0d`: friction damping
   `v -= v >> k` (k from table at `[0xea49 + surface>>4]`), then a contact impulse (`0x4a71`) along the
   terrain normal (from slope angles `[0xea24]`, `[0xea26]` via sin/cos `0x2aad`/`0x2ad8`).
3. Map-edge walls at 0x80 / 0xfe80 (in 8.8 cells): the same impulse with axis normals.
4. `pos += vel`.

Contact impulse `0x4a71`: vn = v·n; if vn < 0: v -= 1.25·vn·n (restitution 0.25), and pos is
moved by 2·vn·n. If vn < −6.0 it calls `0x5864` (impact sound, see below). Then tangential friction
(next section).

Terrain normal: four Q31 values A=`[0xe9a0]`, B=`[0xe9a4]`, C=`[0xe9a8]`, D=`[0xe9ac]` (sin/cos of the
two slope angles), n = (−A, −C, B·D). Tangent velocities: tx = vx·B + vz·A, ty = vy·D + vz·C.

## Wheels, drivetrain, steering (`0x0d2a`–`0x1323`, `0x4b26`–`0x4d95`)

Angles: 0x10000 = full turn (0x4000 = 90°). Sine table (Q15) at DS `0xd10c`, sin `0x2aad`,
cos `0x2ad8`, atan2 `0x2b08`. Fixed-point multiplies are `imul` + `shl edx,1` (Q31).

**Car control words** (car struct): `+0x0a` steer, `+0x0c` throttle, `+0x0e` bit 0 = brake,
`+0x16`/`+0x18` per-step copies, `+0x42..+0x4e` wheel spin for atoms 0–3 (i32), `+0x10..+0x14` car centre
(midpoint of atoms 4 and 6, used by the camera).

**Keyboard** (`0x0d2a`, key table at DS `0x5ad9`, 6 scancodes per player:
throttle, reverse, left, right, brake, unused):
P1 arrows + numpad 5, P2 W/Z/A/D/S, P3 I/M/J/L/K, P4 T/V/F/H/G, P5 numpad 7/9/1/3/Del.
- Throttle: ±50/step (×4 when reversing direction), clamp ±0x2000, returns to 0 by 300/step when no key.
- Steer: rate = 0x40 − (|spin0 + spin1| >> 16) (**speed-sensitive**), ×4 when steering back past centre,
  clamp ±0x2000 (±45°), returns to centre by 0x50/step.

**Drive** (`0x1004`, once per step per car): every spin −= spin>>7 (rolling drag); brake: spin −= spin>>2;
torque = (throttle·0x4000)>>16, then by header word `+0x08`:
- 0: atoms 2,3 get torque<<4 (CAR3 — rear-wheel drive, see below),
- 1: atoms 0,1 get torque<<4,
- ≥2: all four get torque<<3 (4WD, half each; CAR1/2/4/5).

**Axle vectors** (unit, 16.16): side S = norm(atom7 − atom6), forward F = norm((a4−a6) + (a5−a7)).
Atoms 0,1 are **steered**: their axis is F·cos(steer) − S·sin(steer) (`0x1136`); atoms 2,3 use F (`0x10b6`).
So atoms 0,1 (radius 26, y = −80) are the front wheels and the car's front is −y.

**Wheel contact** (`0x4b26`, atoms with type 1/2 touching ground):
1. slip = (tx, ty) + 2·spin·(sin θ, −cos θ), θ = heading of the wheel axis projected on the terrain (`0x4c26`).
2. `0x4c68` grip test, per surface s = `[0xea28]`>>4: budget = min(−vn, maxLoad[s]) / grip[s]
   (grip: words at DS `0xea29`, maxLoad: dwords at `0xea59`). If budget ≤ |slip|₁/64 the wheel **slides**:
   spawn a dirt particle (`0x4d96`) and scale slip by 2/grip[s].
3. The slip component along the wheel's rolling direction is fed into the wheel spin (`[0xe9da]`); the rest is
   removed from the atom (v −= slip, pos −= slip, through B and D).
4. Spin is written back by `0x0f67`: non-driven wheels take the new value; driven wheels blend
   new + (old − new)/4 (exact per-mode rule in the asm — verify signs in testing).

**Surfaces.** The physics height sampler `0x532e` also reads `COL[y][x]` into `[0xea28]`; material
s = colour >> 4 (16 materials × 16 shades). Slope angles are atan2(Δh, 256) of the cell's x and y height
deltas (`[0xea24]`, `[0xea26]`). Tables per material s = 0…15:

| s | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| grip divisor (`0xea29`) | 16 | 25 | 20 | 40 | 30 | 25 | 16 | 20 | 40 | 25 | 25 | 25 | 16 | 25 | 16 | 16 |
| damping shift k (`0xea49`) | 12 | 9 | 10 | 10 | 10 | 10 | 8 | 10 | 10 | 10 | 10 | 10 | 8 | 6 | 10 | 10 |
| max load (`0xea59`, 16.16) | 3.0 | 1.5 | 2.0 | 0.75 | 2.0 | 2.0 | 2.0 | 2.0 | 2.0 | 2.0 | 2.0 | 2.0 | 1.0 | 1.0 | 1.0 | 1.0 |

Gravity: the data segment holds 0x0c00 at `[0x6a]`, but startup (`0x00e2`) overwrites it with 0x1800.

Body atoms (type 0) on the ground: same grip test without a wheel (`0x4cc3`): stick if under budget,
else slide with slip/(grip/2) and spray dirt. Friction damping before contact: `v −= v >> k[s]`
(table at `0xea49`).

Springs (`0x4e0a`–`0x5008`), for each spring a→b:
- d = (pa + va) − (pb + vb) (predicted separation), L = |d| × 64.
- **Type 1 (rigid, plastic):** e = rest − L.
  - e > `sim.cfg[0]` (compressed by more than 32 units): **yield** — rest := rest − e/16 (both words), correct by e/16.
  - e < `sim.cfg[1]` (stretched by more than 64 units): **break** — set bit 7 of the type, skip from then on.
  - else: full correction Δv = d·e/L, split va += Δv >> (s+1), vb −= the rest.
- **Type 0 (slack link):** no force inside [min, max]; outside, correct halfway to the bound.
- **Type ≥ 2 (damped spring, suspension):** corr = (rest − L)/(type/2) + (prev − L)/2, prev := L.

Car–car collision (not ported): `0x500b` checks car pairs whose first atoms are within 0x200 on each axis, then `0x51bd` (not decoded).

## Terrain deformation (`0x0bbf`–`0x0c6c`)

Dirt particles (28-byte records at `[0x3e53]`, max 200, life 50 ticks) are spawned by `0x4d96`
whenever a contact slides, tagged with the ground colour under the atom. Only particles tagged
**exactly 0x0f** fall (gravity/2) and dig; the rest drift. **No shipped COL.PCX contains colour 0x0f, so
the demo never deforms the terrain.** A landing particle:
- darken the colour map: `COL[y][x]` −1 if its low nibble ≠ 0 (tyre/dirt marks),
- **lower the height map** by one step at the cell `(x+0x80, y+0x80)`: `MAP -= 1` (= 16 height units;
  `dec byte`, so 0 wraps to 255),
- re-tags itself 1 and stops. Darkening 0x0f → 0x0e is what stops a cell from digging again.

Quirk: `and cx, 0xff` at `0x4ece` never sets SF, so a broken rigid spring (type 0x81) is not skipped:
it falls into the damped branch as a very soft spring (corr = e/64) that still enforces min/max.

## Still open

- Which material is which (grass, road, water…): compare COL.PCX colour rows with MAPTEX.PCX.
- Exact sign conventions in the wheel slip → spin transfer (step 3/4 above).
- `0x532e` vs `0x25c5` (two height samplers: maybe one includes the car-under-car case).
- Car–car collision `0x5091` / `0x51bd`.
- Step rate (timer setup at startup) — needed for real-time speed.

## Sound (`0x57e0`–`0x592c`)

AdLib / OPL2 FM at port `0x388` (`0x58fc` writes register `ah` = `al`); nothing else (no Sound
Blaster DSP, PC speaker, music or samples). `0x57e0` initialises the chip, `0x588b` silences all
channels (`0xb0`–`0xb8`). The only effect is the ground-impact thud: `0x4a71` calls `0x5864` with
ax = 0 when vn < −6.0 (`cmp ebx, 0xfffa0000`, preceded by two `nop`s). `0x5864` loads a 16-byte FM
patch (`[0xecd9 + ax·16]`, data segment, not decoded yet) into channel 8 via `0x589b` (operator
registers `0x20`/`0x40`/`0x60`/`0x80`/`0xe0` + `[cs:0x592c + 8]` offset), then sets `0xa8`/`0xb8`
(frequency, key-on).

## Fog and smoke tables (`FOG.DAT`, `SMOKE.DAT`)

Both are palette remap tables (256 bytes each: colour in → colour out).

- **`FOG.DAT`**: 18 tables, one per distance level. `0x1cde`/`0x1e3a` pick the level from a
  polygon's depth: `cx = (z₁ + z₂) / 4`, and if `cx ≥ [0x5f5]` the table page is `0x1d + (cx >> 8)`,
  so level = average depth / 512 world units. Levels 0–4 are the identity; 5–16 blend toward sky
  blue (92, 188, 252); 17 is that colour everywhere. So fog starts ~10 cells out and is total by
  ~34. Reformers does the same blend in a shader.
- **`SMOKE.DAT`**: 4 tables of translucency, not dust. `0x2de0` darkens the car's shadow spans
  through table 0; `0x38f8` treats polygon colours `0xf0`–`0xf3` as "see-through", remapping the
  pixel behind through table `colour − 0xf0`: 0 darkens a lot (average brightness ×0.64), 1 a
  little (×0.85), 2 lightens a little, 3 more (×1.2). Every car uses `0xf0` (the windows).
  Dust stays as the sim's dirt particles.
