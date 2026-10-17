---
id: ter-rfb7
status: closed
deps: []
links: [ter-hf1h]
created: 2026-10-17T08:23:03Z
type: feature
priority: 2
assignee: Bence Kiglics
parent: ter-88dh
tags: [ui]
---
# Options overlay on Esc (resume, graphics, quit)

Esc opens an options overlay instead of quitting the game. It holds Resume, a Graphics section and Quit, so graphics settings (starting with ter-0i1a nearest-neighbour sampling) have a place to live.

## Design

In src/bin/play.rs. Esc toggles an overlay drawn over the frozen 3D view (dim the scene with a translucent rectangle). Pause the sim while it is open: skip the 120 Hz tick loop and drop accumulated time so the car does not jump on resume. Navigation with Up/Down + Enter (Esc again = Resume); mouse clicks optional. Items: Resume, Graphics (sub-list of toggles, each showing its current state), Quit. Keep the settings in one struct (e.g. GraphicsSettings) so later options are one field + one menu row. Driving keys must not reach the sim while the overlay is open. No persistence to disk for now.

## Acceptance Criteria

Esc opens the overlay and pauses the simulation; Esc or Resume closes it and the car continues from where it was. Quit exits the game. The Graphics section lists at least one toggle and reflects its state. Arrow keys navigate the menu without steering the car.


## Notes

**2026-10-17T08:23:55Z**

Agent-produced 2026-10-17: scope extended at the user's request with more Graphics rows: fullscreen on/off, window resolution (cycle presets, windowed only) and textures on/off (untextured = shaded flat colours for terrain and car).

**2026-10-17T08:28:59Z**

Agent-produced 2026-10-17: done. Esc toggles a paused overlay (sim stops, no banked time); Up/Down + Enter/Left/Right; rows: Resume, Graphics (texture filter, textures, CRT, fullscreen, resolution 960x600-1920x1200 windowed), Quit. Driving keys are ignored while open. Settings live in a Graphics struct, no persistence.
