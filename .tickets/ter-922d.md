---
id: ter-922d
status: closed
deps: []
links: []
created: 2026-10-16T13:36:54Z
type: task
priority: 1
assignee: Bence Kiglics
parent: ter-88dh
tags: [assets, ]
---
# Phase 1: PCX image decoder

Implement a parser for the 8-bit PCX images used in TEREP2 (map.pcx, col.pcx, maptex.pcx, car[1-5].pcx, textures.pcx). The parser must handle RLE decompression and extract the 2D data as a 2D array or raw buffer.

## Design

Use Python (for prototyping) or C++ (for integration). Read the image size from the PCX header (don't hardcode it) and RLE-decompress the data. Return palette indices plus the 256-colour VGA palette stored at the end of the file. Sizes: MAP/COL/MAPTEX/MAPHILL.PCX are 320x256; CAR*.PCX and TEXTURES.PCX are 320x200. In the 320x256 files only columns 0-255 are the map; columns 256-319 are a sidebar (legend / label / filler) to crop away.

## Acceptance Criteria

A script/binary that reads MAP.PCX and outputs a 256x256 array of palette indices (sidebar cropped), and decodes COL.PCX to a 256x256 RGB image that matches the original.


## Notes

**2026-10-16T13:58:00Z**

Done in Rust (agent-produced, 2026-10-16): src/pcx.rs decode/crop/to_rgb; CLI 'cargo run -- <in.pcx> <out.pgm|out.ppm> [--map]'. MAP.PCX -> 256x256 indices (PGM), COL.PCX -> 256x256 RGB (PPM), visually matches original. Tests: cargo test.
