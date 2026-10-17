---
id: ter-ci66
status: closed
deps: [ter-cs33, ter-qeve, ter-jki2]
links: []
created: 2026-10-16T13:36:55Z
type: task
priority: 1
assignee: Bence Kiglics
parent: ter-88dh
tags: [interaction, ]
---
# Phase 3: Drivable prototype (input + physics + renderer)

Connect the physics engine to the renderer and keyboard controls.

## Design

Implement standard input handling for the vehicle (Arrows/NumPad). Synchronize the physics loop and the rendering loop.

## Acceptance Criteria

A playable prototype where the user can drive and deform the terrain visually.


## Notes

**2026-10-17T08:08:52Z**

Agent-produced 2026-10-17: done. 'cargo run --release --bin play [-- --soft] [--car N]': arrows, space brake, R upright, 1-5 car, W wireframe. Fixed 120 Hz sim, free-running render, chase camera, speed/dent HUD. Terrain deformation visible with --soft (original never digs on shipped maps). Verified via scripted screenshot runs, not yet hand-played.
