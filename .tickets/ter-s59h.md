---
id: ter-s59h
status: closed
deps: [ter-922d]
links: []
created: 2026-10-16T13:36:54Z
type: task
priority: 1
assignee: Bence Kiglics
parent: ter-88dh
tags: [assets, ]
---
# Phase 1: Height map and colour map to float buffers

Convert the 256x256 height map (MAP.PCX, columns 0-255) into a normalizable float array (0.0 to 1.0) that can be used as a Vertex Buffer or Texture in modern graphics APIs.

## Design

MAP.PCX does not use a grayscale palette: use the raw palette index (0-255) as the height, not the palette colour, and divide by 255.0. The sidebar legend shows a gray ramp plus green and red bands; check whether those index ranges mean something other than height (e.g. surface type) before treating all 256 values as height. Prepare a 256x256 RGB map from COL.PCX. MAPHILL.PCX has the same layout and is probably an alternate height map.

## Acceptance Criteria

Raw binary 256x256 float array generated for MAP.PCX and a 256x256 RGB buffer for COL.PCX.


## Notes

**2026-10-16T15:17:29Z**

Agent-produced 2026-10-16: done. src/terrain.rs (Terrain::load, height_at with the game's triangle split, material_at = COL>>4, heights_f32, colour_rgb). Per the EXE all 256 MAP indices are height (index*16 world units); the green/red legend bands are not special in physics. CLI exports: 'cargo run -- MAP.PCX out/map.f32 --map', 'cargo run -- COL.PCX out/col.rgb --map'.
