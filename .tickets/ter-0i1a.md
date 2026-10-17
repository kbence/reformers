---
id: ter-0i1a
status: closed
deps: [ter-rfb7]
links: []
created: 2026-10-17T08:23:12Z
type: feature
priority: 2
assignee: Bence Kiglics
parent: ter-88dh
tags: [graphics, ui]
---
# Nearest-neighbour texture sampling toggle

A graphics option that switches between smooth (linear) and nearest-neighbour sampling, for a crisp pixel look closer to the DOS original. Lives in the Graphics section of the options overlay (ter-rfb7).

## Design

Today the terrain colour is per-vertex (COL.PCX index -> palette RGB, multiplied by slope shading), so it is always interpolated, and the car skins are hard-wired to FilterMode::Nearest. To make the toggle mean something for the terrain: upload COL.PCX (256x256, palette-resolved) as a Texture2D and give each terrain vertex a uv at its cell; keep the slope shading in the vertex colour (macroquad multiplies texture by vertex colour). Then the option just calls set_filter(Nearest|Linear) on the terrain texture and every car skin. Re-upload the colour texture when digging darkens COL cells (the sim decrements colour on landing). Default: Nearest for skins as now; pick the terrain default after seeing both.

## Acceptance Criteria

Toggling the option in the overlay visibly switches terrain and car skins between blocky per-cell/per-texel colours and smooth filtering, without restarting. Dug colour marks still show up on the terrain.


## Notes

**2026-10-17T08:28:59Z**

Agent-produced 2026-10-17: done. Terrain colour now comes from a COL texture (uv at cell centres, slope shade in vertex colour); the option switches it, MAPTEX and the car skins between Nearest and Linear live. Default nearest. Related: textures on = MAPTEX.PCX tiles per cell via a terrain shader (tile = row colour>>4, column colour&15), textures off = flat COL colours; car panels/wheels take the skin colour at their uv centre.
