# Reformers

A Rust remake of **-DEFORMERS-** (TEREP2), the 1996–97 DOS soft-body vehicle simulator by
Nagymáthé Dénes. The physics is a literal 16.16 fixed-point port of the original: cars are point
masses joined by springs that yield and break, driven over a 256×256 height map.
How the original works: [docs/re-notes.md](docs/re-notes.md).

## Play

```bash
cargo run --release
```

| Key | Action |
|---|---|
| ↑ / ↓ | throttle / reverse |
| ← / → | steer |
| Space | brake |
| R | put the car back on its wheels (time trial: on the last checkpoint, facing the next) |
| 1–5 | switch car |
| W | spring wireframe |
| Esc | options: resume, graphics, quit (pauses the game) |

Graphics options: texture filter (nearest/smooth), textures on (MAPTEX.PCX material tiles, car
skins) or off (flat COL.PCX colours), CRT filter, minimap, ghost car, fullscreen, window resolution.

Flags: `--car N` starting car; `--soft` makes all ground diggable so wheels dig tracks (the original
only digs ground of colour 0x0f, which no shipped map has); `--smooth`, `--untextured`, `--crt`,
`--no-minimap`, `--ghost` preset the options; `--menu` starts paused.

## Time trial

When `assets/CHECK.PCX` exists, the map is a time trial. Drive through the start line to start the
clock, then through the checkpoints in order; any other order doesn't count. The next checkpoint is
the yellow arch (start/finish has a chequered banner); the others are dim. Each arch stands on the
two ends of its painted line. Best laps are saved per map and car in
`~/.local/share/reformers/best_laps.txt` (or `$XDG_DATA_HOME`, or `%APPDATA%` on Windows). The
best lap can also drive along as a translucent ghost car with a dot on the minimap (off by default;
Esc menu or `--ghost`), saved next to it as `ghost-*.bin`.

`CHECK.PCX` is a 256×256 indexed image on the same grid as `MAP.PCX`/`COL.PCX` (the 64 px strip on
the right is a legend and is ignored), so tracks can be painted in any paint program. Like
`COL.PCX`, colour index ÷ 16 is the element type and index mod 16 its setting:

| Indices | Element | Setting |
|---|---|---|
| 0–15 | nothing | – |
| 16–19 | spawn: a small blob, the car starts at its centre | facing up / right / down / left |
| 32–47 | checkpoint: paint a line across the road (1 px is enough) | lap order; the lowest is start/finish, gaps allowed, up to 16 |
| 48–63 | reset zone: driving in puts the car back like R (paint it over cliffs and water) | – |
| 64–79 | boost pad (cyan on the ground) | strength |
| 80–95 | slow zone, mud (brown on the ground) | strength |
| 96–255 | reserved | – |

Respawns put the car on a checkpoint's centre, so centre each line on the drivable road.

The game watches `MAP.PCX`, `COL.PCX` and `CHECK.PCX` while it runs: save one in your paint program
and the map reloads within a second (the time trial restarts). A file that fails to load shows an
error and the previous map stays.

## Assets

The game reads the original demo's files from `assets/` (also found next to the executable, or
wherever `REFORMERS_ASSETS` points): `MAP.PCX`, `COL.PCX`, `MAPTEX.PCX`, `TEXTURES.PCX`,
`CAR1–5.DAT`, `CAR1.PCX`, `CAR2.PCX` (`MAPHILL.PCX` is an unused alternate height map). They are the
original author's copyrighted work; see `assets/READ.ME`. `CHECK.PCX`, the time-trial track, is
Reformers' own.

## Tools

```bash
cargo run --release --bin tool -- drive "t:3 tl:2 b:1" out/run   # headless drive → out/run.pgm, out/run_dents.pgm
cargo run --release --bin tool -- assets/MAP.PCX out/map.pgm --map  # PCX → PGM/PPM/raw f32/raw RGB
cargo test
```

Issues are tracked on GitHub: https://github.com/kbence/reformers/issues
