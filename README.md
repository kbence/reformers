# Reformers

A Rust remake of **-DEFORMERS-** (TEREP2), the 1996–97 DOS soft-body vehicle simulator by
Nagymáthé Dénes. The physics is a literal 16.16 fixed-point port of the original: cars are point
masses joined by springs that yield and break, driven over a 256×256 height map.
How the original works: [docs/re-notes.md](docs/re-notes.md).

## Play

```bash
cargo run --release
```

| Key | Gamepad | Action |
|---|---|---|
| ↑ / ↓ | right trigger or A / left trigger or X | throttle / reverse |
| ← / → | left stick or d-pad | steer |
| Space | B | brake |
| R | Y | put the car back on its wheels (time trial: on the last checkpoint, facing the next); dents stay |
| F | Back | repair the car (a fresh one where it stands) |
| 1–5 | | switch car |
| C | right shoulder | camera: chase / bumper / orbit (Q/E rotate) / top-down |
| W | | spring wireframe |
| Esc | Start | options (pauses the game); d-pad and A in the menu |

Options (saved to `settings.txt` next to the best laps; flags override them): sound volume, camera, fog (the original's distance fog), texture filter (nearest/smooth), textures on (MAPTEX.PCX material tiles, car
skins) or off (flat COL.PCX colours), CRT filter, minimap, ghost car, fullscreen, window resolution.

Flags: `--car N` starting car; `--soft` makes all ground diggable so wheels dig tracks (the original
only digs ground of colour 0x0f, which no shipped map has); `--smooth`, `--untextured`, `--crt`,
`--no-minimap`, `--ghost`, `--no-fog`, `--mute` preset the options; `--map NAME` picks a map; `--menu` starts paused.

## Time trial

When a map has a `CHECK.PCX`, it's a time trial. Drive through the start line to start the
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
| 96–111 | soft dirt: wheels dig ruts on every pass, up to 6 height steps deep; they stay until the map reloads | – |
| 112–255 | reserved | – |

Respawns put the car on a checkpoint's centre, so centre each line on the drivable road.

The game watches the current map's `MAP`, `COL` and `CHECK` files while it runs: save one in your paint program
and the map reloads within a second (the time trial restarts). A file that fails to load shows an
error and the previous map stays.

## Maps and assets

Each map is a folder in `assets/maps/`: `MAP` (heights) and `COL` (colours = surface materials), as
PCX or TEREP1's BMP, plus an optional `CHECK.PCX` that makes it a time trial. Pick one in the Esc
menu or with `--map NAME`; the last one is remembered. Shipped: `terep2` (the original, with the
time-trial track), `terep2-hills` (its unused `MAPHILL.PCX`) and `terep1` (TEREP1's map, same
landscape, own palette; heights use TEREP2's scale, so it comes out gentler). A map folder may
carry its own `MAPTEX.PCX`; otherwise the shared one is used when the palettes match, and the
terrain is drawn in flat colours when they don't.

Shared game files live in `assets/` (also found next to the executable, or wherever
`REFORMERS_ASSETS` points): `MAPTEX.PCX`, `TEXTURES.PCX`, `CAR1–5.DAT`, `CAR1.PCX`, `CAR2.PCX`.
They and the map images are the original author's copyrighted work; see `assets/READ.ME`. The
`CHECK.PCX` time-trial track is Reformers' own.

## Tools

```bash
cargo run --release --bin tool -- drive "t:3 tl:2 b:1" out/run   # headless drive → out/run.pgm, out/run_dents.pgm
cargo run --release --bin tool -- assets/maps/terep2/MAP.PCX out/map.pgm --map  # PCX → PGM/PPM/raw f32/raw RGB
cargo run --release --bin tool -- sounds out/sounds                 # the synthesised sounds as WAVs
cargo test
```

Sound is synthesised at start-up, no sample files: the original's two AdLib FM patches (the engine,
retuned with wheel speed, and the landing thud, played 3 octaves up so speakers can reproduce it),
plus skid noise and checkpoint/lap chimes.

## Web

```bash
web/build.sh                              # needs: rustup target add wasm32-unknown-unknown
python3 -m http.server -d out/web 8000    # then open http://localhost:8000
```

The browser build loads every asset over HTTP at start (`assets/maps/index.txt` lists the maps,
since the web can't list folders). No gamepads, no saved settings or best laps, and no hot reload
there; the rest, CRT filter included, works the same. Hosting it publicly would hand out the
original game files (see #1).

## Releases

Publishing a GitHub release runs `.github/workflows/release.yml`: it tests and builds Windows,
macOS (universal) and Linux, and attaches an archive per platform (binaries, `assets/`, this
README). Run the workflow by hand to build without a release; the archives are kept as artifacts.

Issues are tracked on GitHub: https://github.com/kbence/reformers/issues
