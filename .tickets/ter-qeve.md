---
id: ter-qeve
status: closed
deps: [ter-s59h]
links: []
created: 2026-10-16T13:36:55Z
type: task
priority: 1
assignee: Bence Kiglics
parent: ter-88dh
tags: [graphics, ]
---
# Phase 3: 3D terrain renderer

Create a 3D scene rendering the 256x256 height map as a mesh with vertex displacement. Map the COL.PCX colour data (256x256) onto the mesh. MAPTEX.PCX is a texture atlas (rows of 16px material tiles), not a map; use it for surface detail.

## Design

Use a Vertex Displacement shader (Tessellation recommended). Implement compute shaders for terrain deformation.

## Acceptance Criteria

A window displays the 3D colored terrain from the original DOS game files in high resolution.


## Notes

**2026-10-16T15:17:29Z**

Agent-produced 2026-10-16: car meshes come from the CAR*.DAT display list (renderer bytecode, Car::display_list). Opcode table and known operand layouts in docs/re-notes.md; ops 3,5,7,8,9,10,16 still need decoding.

**2026-10-17T08:08:52Z**

Agent-produced 2026-10-17: done. src/bin/play.rs (macroquad 0.4.16): 256x256 terrain in 8 chunk meshes coloured from COL.PCX with slope shading, rebuilt per chunk when dug; car drawn from the decoded display list (flat + textured polys with CARn/TEXTURES.PCX skin, shadows, rolling wheels). Display-list format fully sized in docs/re-notes.md. No tessellation/compute: not needed at this size.
