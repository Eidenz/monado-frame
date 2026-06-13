# monado-frame

An **in-headset overlay** for Monado — review screenshots and configure the
finger-frame gesture, with no desktop UI. It's a standalone OpenXR overlay app
(`XR_EXTX_overlay`), like [WayVR](https://github.com/galister/wayvr), and a
companion to the gesture/screenshot features in the Monado fork.

It stays fully decoupled from `monado-service` through two files:

- **writes** `~/.config/monado/gestures.json` — the gesture detector hot-reloads it (toggle, hold delay…);
- **watches** `~/Pictures/Monado` — any Monado screenshot (gesture, controller chord, or `SIGUSR1`) lands there and is shown for review.

## Build & run

```bash
cargo build
cargo run        # run it while your Monado / Envision VR session is active
```

Requires an active OpenXR runtime (Monado) exposing `XR_KHR_vulkan_enable2` and
`XR_EXTX_overlay`. Logs go to stdout (`RUST_LOG=debug` for more).

## Controls

Everything is driven by the controllers; there is no desktop window.

- **Settings panel** — hidden on launch. **Double-press SYSTEM** (within 400 ms)
  to toggle it. Double-tapping right-SYSTEM opens-then-closes WayVR (net no
  change) while toggling this once, so the two coexist. It spawns in front of
  wherever you're looking.
- **Point & click** — aim a controller at a panel; the trigger is the click.
- **Move a panel** — point at it and squeeze the **grip** (force) to grab; it
  rides your hand until you release.
- **Screenshots** — a new shot queues a **notification card** on your left wrist
  (mini preview + date). It's hand-locked and only appears when you **turn your
  wrist so the card faces you**. Use ‹ › to scroll the queue; click the preview
  to open that shot as a floating photo window (copy / delete / close). Up to
  **3** photo windows can be open at once.
- **Gallery** — open it from the **Open gallery** button in settings: a paged
  grid of every screenshot. Click a thumbnail to open it as a floating window.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `MONADO_SCREENSHOT_DIR` | `~/Pictures/Monado` | Folder watched for new screenshots. |
| `MONADO_FRAME_OPACITY` | `0.92` | Panel glass opacity (0–1). |
| `MONADO_FRAME_NO_ALPHA` | unset | Set to disable alpha blending (opaque rectangular panels) if glass looks wrong. |
| `MONADO_FRAME_NO_LASER` | unset | Set to disable the 3D laser pointer. |
| `MONADO_FRAME_WRIST_POS` | `-0.05,0.01,0.05` | Watch position offset `x,y,z` (metres) from the left grip pose. |
| `MONADO_FRAME_WRIST_ROT` | `90,180,63` | Hand-locked orientation `yaw,pitch,roll` (degrees). |
| `MONADO_FRAME_WRIST_FOV` | `35` | Reveal half-angle (degrees): the card shows while its face points within this of your head. |

The wrist card follows the left controller's **grip** pose with a fixed
hand-locked orientation, and appears only when you turn your wrist so the card's
face points toward your head (within `MONADO_FRAME_WRIST_FOV`). Adjust
`MONADO_FRAME_WRIST_POS` to place it; to find the axes, run once with
`MONADO_FRAME_WRIST_POS=0,0,0` (card at the palm) and bump one axis by `0.1` to
see which way it moves.
