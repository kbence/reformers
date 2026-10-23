# Reformers: notes for agents

Rust remake of TEREP2 (macroquad). The README covers playing, maps, the editor and tools;
`docs/re-notes.md` covers the original program. Commits go straight to `main`, as
Conventional Commits (`fix:`, `feat:`, `refactor:`, `docs:`) with a body saying what was
wrong, what changed and how it was checked.

## Checks

- `cargo test --release` before committing. `scripted_drive_is_bit_exact` pins the physics to
  one golden hash on every OS: replays and online races depend on it, so a change that moves
  the hash needs a reason in the commit.
- `cargo fmt --check`.
- Toolchain: stable Rust 1.85 or newer (edition 2024).

## Versions and releases

- Tags are `vMAJOR.MINOR.PATCH`, made by publishing a GitHub release on `main`. Publishing runs
  `.github/workflows/release.yml`, which tests and attaches Windows, macOS and Linux archives.
- Below 1.0, MINOR stands for a major change: raise it (0.6.x → 0.7.0) only for one. Bug
  fixes and smaller features raise PATCH (0.6.0 → 0.6.1). Ask the maintainer when unsure.
- Before tagging, set `version` in `Cargo.toml` to the release's number (without the `v`) and
  commit it with the refreshed `Cargo.lock` as `chore: version X.Y.Z`. Releases before v0.6.1
  left it at 0.1.0.
- The online protocol has its own number, `VERSION` in `src/net.rs`. Raise it whenever the
  messages or the sim change so that two builds would disagree, and mention it in the release
  notes: players on different protocol versions can't race each other.
- Release notes: features, then fixes, from `git log --oneline vPREVIOUS..main`; say what
  wasn't tested across real machines (online play especially).
