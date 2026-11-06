# Smooth online motion under lag: research notes

Background research for #70 (adaptive input delay), compiled by a Claude Code research agent on
2026-11-06 and lightly edited: agent-generated, not hand-verified. The Overwatch and Rocket League
details come from secondary or forum sources, not the talks themselves.

## The symptom

Online with `REFORMERS_LAG=15+15` on both sides (120 Hz display), about half the frames ran 0
ticks and the rest 2–3: cars moved in bursts and the chase camera shook (view-direction jerk 2–3×
offline). Lockstep runs a tick only when every player's input for it is in.

## Why it stalls, then bursts

1. **No clock sync.** On a stall the loop keeps up to 100 ms of time owed (`acc.min(0.1)`) and
   pays it back in bursts. Whichever side is ahead sits at the edge of its buffer and runs each
   frame the moment it arrives, so network jitter goes straight to the screen. Under 15 + 0–15 ms,
   the round trip plus a frame of polling on each side is about 47–77 ms, more than the old fixed
   delay of 6 ticks (50 ms).
2. **120 Hz display against a 120 Hz tick.** Frame-time wobble around 8.33 ms gives 0/2-tick
   pairs, offline too (minor: ~27 frames in 20 s here). Fiedler calls it "subtle but visually
   unpleasant stuttering".

## Ranked changes (most smoothness per effort first)

1. **Interpolate drawing between ticks** (render-only, ~40 lines). Draw each car between the
   states before and after the last tick, `alpha = (acc * TICK_HZ).min(1)`; make the camera damping
   frame-rate independent, `cam.lerp(target, 1 - (-6 dt).exp())`, aimed at the interpolated
   centre; snap on resets and joins. Removes the 0/2 aliasing; doesn't fix network stalls.
   Costs at most a tick (8 ms) of display latency.
2. **Clock sync by running slightly fast or slow** (no sim change, a byte in Frames). The host is
   the clock. It reports how many ticks early each joiner's inputs arrive (a minimum over ~1 s);
   the joiner compares that with how many frames it holds ahead and runs up to ~5% faster or
   slower to even them out, instead of stalling and bursting. Precedents: GGPO's
   `recommend_frame_wait_duration`, GGRS ("Skipping frames produces a visible stutter"),
   Overwatch's client speeding up ~5% when the server's input buffer runs low, WebRTC NetEQ's
   time-stretching. Deterministic: only *when* a tick runs changes.
3. **Adaptive input delay** (no sim change; uses the margins from 2). The two margins add up to
   the delays less the round trip, so the delay needed follows from them without timestamps.
   Raise at once, lower slowly (Age of Empires: a consistently slow response beats a jerky fast
   one), clamp to 4–20 ticks. Safe mid-race: a player's delay only decides which tick their next
   input lands on, and the host's Frames are the record.
4. **Playout buffer for drawing** (optional). Draw a smoothly moving time ~1.5 ticks behind the
   newest state, nudging its rate to hold that; hides leftover stalls of up to ~2 ticks for ~12 ms.
5. **The host stops waiting for a late joiner** (optional, for 4 players). Repeat the joiner's
   last input (one-shot bits cleared) instead of stalling everyone, as Factorio's server and
   Rocket League do. Changes gameplay for the laggy player; still deterministic.
6. **Rollback (GGPO-style): not now.** CPU is cheap (~2–3 µs per car per tick, so resimulating 10
   ticks for 4 cars is ~0.1 ms), but saving terrain state every tick, remote cars snapping on
   mispredictions, duplicate sounds, and confirmed-only replays cost a lot. It buys lower input
   lag, not smoothness, and still needs clock sync. Revisit for internet play at 100 ms input lag
   or more than 4 players.

## Sources

- Age of Empires, "1500 Archers on a 28.8": https://www.gamedeveloper.com/programming/1500-archers-on-a-28-8-network-programming-in-age-of-empires-and-beyond
- Fiedler, "Deterministic Lockstep": https://gafferongames.com/post/deterministic_lockstep/
- Fiedler, "Fix Your Timestep": https://gafferongames.com/post/fix_your_timestep/
- GGPO `timesync.cpp`: https://github.com/pond3r/ggpo/blob/master/src/lib/ggpo/timesync.cpp
- GGRS time sync: https://docs.rs/crate/ggrs/latest/source/docs/time-synchronization.md
- Overwatch netcode (secondary): https://edgegap.com/blog/game-backend-deep-dive-overwatch-2016-netcode-architecture-rollback (talk: https://gdcvault.com/play/1024001/)
- Rocket League input buffer (developer forum post): https://devtrackers.gg/rocket-league/p/bb42f194-rocket-league-netcode-question-is-your-local-client-constantly-extrapolating-the-ball-other-players (talk: https://www.gdcvault.com/play/1024972/)
- Factorio FFF-302: https://factorio.com/blog/post/fff-302 and FFF-83: https://factorio.com/blog/post/fff-83
- StarCraft turn rate: https://liquipedia.net/starcraft/Ladder/Alternate_IDs/Battle.net/Latency
- WebRTC NetEQ: https://webrtc.googlesource.com/src/+/HEAD/modules/audio_coding/neteq/g3doc/index.md
- Driscoll, frame-rate-independent damping: https://www.rorydriscoll.com/2016/03/07/frame-rate-independent-damping-using-lerp/
