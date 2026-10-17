---
id: ter-8quv
status: closed
deps: [ter-rfb7]
links: []
created: 2026-10-17T08:24:48Z
type: feature
priority: 3
assignee: Bence Kiglics
parent: ter-88dh
tags: [graphics, ui]
---
# CRT filter graphics option

A nostalgia post-process: scanlines, slight barrel curvature, vignette and RGB mask, toggled from the Graphics section of the options overlay (ter-rfb7).

## Design

Render the 3D scene into a render target with a depth buffer, then draw it to the screen through a macroquad Material with a CRT fragment shader (GLSL 100 so it also runs on GLES2/WebGL). HUD and overlay draw after the post-process so text stays sharp. Off by default.

## Acceptance Criteria

Toggling CRT in the overlay switches the view between clean and CRT-styled without restarting; HUD/menu text stays readable.


## Notes

**2026-10-17T08:28:59Z**

Agent-produced 2026-10-17: done. Scene renders to a depth render target, then a GLSL 100 CRT shader (barrel, 3 px scanlines, RGB aperture mask, vignette) draws it to the screen; HUD and overlay draw after, unfiltered. Off by default.
