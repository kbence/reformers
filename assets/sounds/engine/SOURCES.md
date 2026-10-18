# Engine loops: where they come from

| File here | Original file | SHA-256 |
|---|---|---|
| `loop_0.wav` | https://opengameart.org/sites/default/files/loop_0.wav | `69d74b106509037ce547429e4c3f9ae906b0fffe88d0bb8b3b5a8b0cfc239048` |
| `loop_1.wav` | https://opengameart.org/sites/default/files/loop_1_0.wav | `0bc6bf7e1ad33d347ed805b4f2b55d1f79e38f892006f98acbe85a3177ff77a2` |
| `loop_2.wav` | https://opengameart.org/sites/default/files/loop_2_0.wav | `e26bef6f1804670aba02b6b9a8c5485cf05a6a1e902a5a50c098230ace49967f` |
| `loop_3.wav` | https://opengameart.org/sites/default/files/loop_3_0.wav | `015aee5cd2dec394b28ff0994360c780cfbe29c3cddfb16dba8df81b4a68eea2` |
| `loop_4.wav` | https://opengameart.org/sites/default/files/loop_4_0.wav | `fadf4420104d9273e6cc0a1c4583d863937af382382866251fc2b8dc005f9369` |
| `loop_5.wav` | https://opengameart.org/sites/default/files/loop_5_0.wav | `e0c65476764931770e46308dec744a196d8ecf996f8c7675f125805d28609b34` |

- **Work:** "racing car engine sound loops" by **domasx2**, published on OpenGameArt on 2011-07-24
  (made for Bandit Racer, www.banditracer.eu): https://opengameart.org/content/racing-car-engine-sound-loops
- **Licence:** CC0 1.0 (public domain dedication), as shown on that page:
  https://creativecommons.org/publicdomain/zero/1.0/
- **Underlying recording:** the author cut the loops from a public-domain recording on pdsounds.org,
  "start_up_car_with_seat_belt_warning_beep"
  (http://www.pdsounds.org/sounds/start_up_car_with_seat_belt_warning_beep), edited them to loop and
  pitch-shifted them; the six files differ only in pitch (measured: ~43, 61, 65, 69, 72, 77 Hz).
- **Licence history:** on 2011-07-24 a commenter questioned whether an earlier source sample
  (sampling+ licence) allowed release; the author replied the same day that the loops had been
  remade from a public-domain sample. The page lists CC0.
- **Changes:** none to the files. They are byte-identical to the downloads (checksums above,
  downloaded 2026-10-18), only renamed to `loop_N.wav`, slowest to fastest. The game currently uses
  `loop_0.wav` alone and pitches it up at load time (resampled to 90–320 Hz firing rate, since the
  recorded ~43 Hz is below what most speakers reproduce); `loop_1`–`loop_5` are kept as published.
