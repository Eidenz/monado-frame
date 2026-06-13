# monado-frame

An **in-headset overlay** for Monado — review screenshots and configure the
finger-frame gesture, with no desktop UI. It's a standalone OpenXR overlay app
(`XR_EXTX_overlay`), like [WayVR](https://github.com/galister/wayvr), and a
companion to the gesture/screenshot features in the Monado fork.

It stays fully decoupled from `monado-service` through two files:

- **writes** `~/.config/monado/gestures.json` — the gesture detector hot-reloads it (toggle, hold delay…);
- **watches** `~/Pictures/Monado` — any Monado screenshot (gesture, controller chord, or `SIGUSR1`) lands there and is shown for review.

## Status

Early — **Phase 2a**: renders a test panel in-headset to validate the overlay
pipeline (OpenXR overlay session + Vulkan + a quad layer). Input, the settings
UI, and screenshot review come next.

## Build & run

```bash
cargo build
cargo run        # run it while your Monado / Envision VR session is active
```

Requires an active OpenXR runtime (Monado) exposing `XR_KHR_vulkan_enable2` and
`XR_EXTX_overlay`. Logs go to stdout (`RUST_LOG=debug` for more).

## Roadmap

- **P2a** — overlay skeleton: a solid test panel in front of you.
- **P2b** — controller aim raycast onto the panel + click.
- **P2c** — settings panel (egui) → writes `gestures.json`.
- **P2d** — screenshot review: grabbable photo panel with copy / delete / dismiss.
- **P2e** — grab/positioning polish + a recent-shots history strip.
