---
id: ter-cs33
status: closed
deps: [ter-s59h]
links: []
created: 2026-10-16T13:36:55Z
type: task
priority: 0
assignee: Bence Kiglics
parent: ter-88dh
tags: [physics, ]
---
# Phase 2: Headless terrain physics and deformation

Implement the core terrain interaction logic. This includes heightmap sampling, suspension calculation, and permanent terrain deformation.

## Design

Use fixed-point logic or maintain the original 8-bit precision to preserve the 'feel' of the mechanics. Starting guess for deformation: H_new = H_old - (Pressure * DeformationRate). The plan's sim.cfg defaults (0x0800, 0xF000) are unverified: no sim.cfg ships in this folder, although the unpacked TEREP2U.EXE references one. Treat the formula and constants as tunable until confirmed.

## Acceptance Criteria

A headless CLI loop that allows moving a vehicle over the map and outputs a modified heightmap where tire tracks are visibly dented.


## Notes

**2026-10-16T14:07:20Z**

Agent-produced 2026-10-16: physics is a mass-spring (atom) sim, not a heightmap-only model. Spring rules, sim.cfg meaning (yield +32 / break -64 units), gravity 0x1800/step, contact restitution 0.25, terrain dig via dirt particles: docs/re-notes.md. Wheel traction 0x4b26-0x4d96 still open.

**2026-10-16T14:25:59Z**

Agent-produced 2026-10-16: wheels/drivetrain/steering/input decoded (docs/re-notes.md 'Wheels, drivetrain, steering'): slip-based grip per COL material, drive mode header+0x08 (0 RWD, 1 FWD, 2 4WD), speed-sensitive steering clamp ±45°, surface tables. Open: slip->spin sign conventions.

**2026-10-16T17:30:21Z**

Agent-produced 2026-10-16: done. src/sim.rs is a literal 16.16 port of the 120 Hz tick (springs, atoms, contacts, wheels, drive, input, dirt particles, digging). Headless CLI: 'cargo run --release -- drive [--soft] [--car N] "t:3 tl:2 b:1" out/run' writes out/run.pgm + out/run_dents.pgm. Finding: the original only digs on COL colour 0x0f, which no shipped map has, so it never dents; --soft emulates an all-0x0f map. Deviations: dent saturates at 0 (asm wraps), car-car collision not ported, trig via f64 instead of the EXE tables.
