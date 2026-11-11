# The README demo

`aibus-ui.gif` is about ten seconds of the dashboard. It is made, not captured by hand.

* `demo/script.ron` is a script for the fake provider: a lead plans, the advisor and router speak, three workers report.
* `tools/record-demo.sh msgs` runs it with the real engine (`aibus run --record`, throwaway `$HOME`, no network, no keychain) and keeps the message stream as `demo/msgs.jsonl`.
* `aibus demo` (hidden subcommand) plays that stream into the real dashboard on a virtual clock: 30 Hz ticks, a typed prompt, text a few characters per tick, two mouse gestures. It is 98x40, the `recording` theme, one frame every 100 ms. It reads no config, clock, tty size or network, so two plays are identical (`crates/aibus/tests/demo.rs`).
* `tools/demo-cast.py` runs `aibus demo` in a pseudo-terminal and writes an asciicast. The demo ends each frame with a private `ESC ] 7777 ; <ms> BEL`, which the script turns into the frame's time, so the cast follows the virtual clock.
* `agg` (asciinema, v1.7.0, release binary, SHA-256 pinned in the script) renders the GIF with DejaVu Sans Mono. `tools/demo-gif-check.py` reads the frame delays and checks length and size.

Regenerate it in a container, never on a host (MARS is the default host of `nas.sh`):

```sh
tools=/Users/rom/workspace/libviprs/tools/nas.sh
$tools push . aibus-r3
$tools run aibus-r3 rust:1.85@sha256:e51d0265072d2d9d5d320f6a44dde6b9ef13653b035098febd68cce8fa7c0bc4 \
  'CARGO_TARGET_DIR=/tmp/target tools/record-demo.sh all'
# copy docs/media/ back from the scratch tree, then
$tools clean aibus-r3
```

The text frames and the cast are deterministic; the GIF pixels depend on agg and the font version the image's Debian ships.

## The rataflow path

The centre column is drawn through rataflow, so the recording above is of that path. `AIBUS_FEATURES` goes straight to `cargo build --features` in `tools/record-demo.sh`, for any feature you want in a recording; nothing else in the script changes. The two plays inside a run have to agree byte for byte (`cmp` on the casts and on the text frames), which is what keeps the drawing deterministic.
