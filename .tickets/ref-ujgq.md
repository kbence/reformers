---
id: ref-ujgq
status: open
deps: []
links: []
created: 2026-10-17T14:16:10Z
type: chore
priority: 3
assignee: Bence Kiglics
parent: ter-88dh
tags: [release]
---
# Prepare for open-sourcing: remove original assets from git

The plan is to open-source Reformers once it plays well. The repo (private, kbence/reformers) currently commits the original DEFORMERS assets, which are the author's copyrighted work (assets/READ.ME), so they must not be in the public repo, including its history.

## Design

Before flipping visibility: (1) gitignore assets/ and stop tracking the files; (2) rewrite history to drop them (git filter-repo --path assets --invert-paths) or publish a fresh-history copy instead; (3) README: explain where to get the original demo and that the game loads files from assets/ or REFORMERS_ASSETS; (4) make tests that need assets skip cleanly when they are missing (CI without assets); (5) pick a licence for the code. Optional: ask the original author for permission to bundle the demo files.

## Acceptance Criteria

A clone of the public repo contains no original game files in any commit; the README tells players how to supply them; cargo test passes without assets (asset tests skipped).

