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
| R | put the car back on its wheels |
| 1–5 | switch car |
| W | spring wireframe |
| Esc | options: resume, graphics, quit (pauses the game) |

Graphics options: texture filter (nearest/smooth), textures on (MAPTEX.PCX material tiles, car
skins) or off (flat COL.PCX colours), CRT filter, fullscreen, window resolution.

Flags: `--car N` starting car; `--soft` makes all ground diggable so wheels dig tracks (the original
only digs ground of colour 0x0f, which no shipped map has); `--smooth`, `--untextured`, `--crt`
preset graphics options; `--menu` starts paused.

## Assets

The game reads the original demo's files from `assets/` (also found next to the executable, or
wherever `REFORMERS_ASSETS` points): `MAP.PCX`, `COL.PCX`, `MAPTEX.PCX`, `TEXTURES.PCX`,
`CAR1–5.DAT`, `CAR1.PCX`, `CAR2.PCX` (`MAPHILL.PCX` is an unused alternate height map). They are the
original author's copyrighted work; see `assets/READ.ME`.

## Tools

```bash
cargo run --release --bin tool -- drive "t:3 tl:2 b:1" out/run   # headless drive → out/run.pgm, out/run_dents.pgm
cargo run --release --bin tool -- assets/MAP.PCX out/map.pgm --map  # PCX → PGM/PPM/raw f32/raw RGB
cargo test
```

Issues are tracked on GitHub: https://github.com/kbence/reformers/issues
