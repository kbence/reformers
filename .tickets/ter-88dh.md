---
id: ter-88dh
status: open
deps: []
links: []
created: 2026-10-16T13:36:39Z
type: epic
priority: 0
assignee: Bence Kiglics
tags: [modernization, ]
---
# TEREP2 modern reimplementation

Reimplement the DOS-based TEREP2 terrain deformation simulator as a modern OpenGL/Vulkan/Metal application on macOS. This covers asset extraction, core physics, rendering, and polishing based on the 'reimplement_terep2_plan.md'.


## Notes

**2026-10-16T13:39:30Z**

Execution order (critical path):
1. ter-922d (PCX parser) + ter-jki2 (vehicle geometry) -- ready now, parallel
2. ter-s59h (heightmap/colormap extraction) -- after ter-922d
3. ter-cs33 (physics) + ter-qeve (renderer) -- after ter-s59h, parallel
4. ter-ci66 (physics + control loop integration) -- after ter-cs33 + ter-qeve
5. ter-hf1h (polish) -- after ter-ci66

**2026-10-16T13:42:15Z**

Critical path update: ter-ci66 (drivable prototype) now also depends on ter-jki2 (car format). Map facts corrected in tickets: maps are 256x256 + 64px sidebar on the right; MAPTEX is a texture atlas; sim.cfg not present.
