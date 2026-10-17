---
id: ter-jki2
status: closed
deps: []
links: []
created: 2026-10-16T13:36:55Z
type: task
priority: 2
assignee: Bence Kiglics
parent: ter-88dh
tags: [assets, ]
---
# Phase 1: Car model format (CAR*.DAT)

Analyze the binary format of car[1-5].dat to extract vertex, face, and skin mapping data for a modern rendering pipeline.

## Design

Use hexdump and pattern matching on the binary files. Estimate bounding boxes or find primitive shapes if format is complex.

## Acceptance Criteria

A JSON/binary intermediate format describing the car models for rendering.


## Notes

**2026-10-16T14:07:20Z**

Agent-produced 2026-10-16: format decoded from the EXE loader (0x2454). Atoms (28 B: pos/vel 16.16, radius, type), springs (14 B: a, b, rest*64, orig/prev, type|shift, min, max), faces. See docs/re-notes.md.

**2026-10-16T15:17:29Z**

Agent-produced 2026-10-16: done for physics. src/car.rs parses atoms, springs, drive mode; the display list is kept as raw bytes. No JSON intermediate: the crate parses .DAT directly. Display-list decoding (opcodes in docs/re-notes.md) moved to ter-qeve.
