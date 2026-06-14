# monado-frame

An **in-headset overlay** for Monado — review screenshots and configure the
finger-frame gesture. It's a standalone OpenXR overlay app
(`XR_EXTX_overlay`), like [WayVR](https://github.com/galister/wayvr), and a
companion to the gesture/screenshot features in my [Monado fork](https://github.com/Eidenz/Monado).

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
- **Screenshots** — a new shot queues a **notification card** on your left wrist
  (mini preview + date). It's hand-locked and only appears when you **turn your
  wrist so the card faces you**. Use ‹ › to scroll the queue; click the preview
  to open that shot as a floating photo window (copy / delete / close). Up to
  **3** photo windows can be open at once.
- **Gallery** — open it from the **Open gallery** button in settings: a paged
  grid of every screenshot. Click a thumbnail to open it as a floating window.
- **QR codes** — enable *Detect QR codes* in settings. New screenshots are
  scanned; if a QR is found, the wrist shows a QR notification instead of the
  photo. Clicking it opens the link (`xdg-open`) or shows the text in a panel.
  Optionally *Delete the screenshot, keep only the code* so QR scans don't
  clutter your gallery.
- **Translate** — on a photo window, the **Translate** button sends the image to
  a vision model that OCRs and translates any text to English, shown in the
  window (toggle back to the image any time). Requires `translate.env` (below);
  the button is hidden when it's not configured.
- **Share** — uploads the screenshot to a [Picsur](https://github.com/CaramelFur/Picsur)
  instance and copies the link to your clipboard. Requires `picsur.env` (below).
- **Auto-cleanup** — a settings slider deletes screenshots older than N days on
  launch (0 = keep forever). The gallery also has a per-thumbnail delete.

## Translation (optional)

The Translate button uses an [Ollama](https://ollama.com) (OpenAI-compatible)
**vision** model — one call does OCR + translation. It's gated by a build-time
config so no endpoint/keys are ever typed in VR: copy `translate.env.example` to
`translate.env`, set your server, and rebuild.

```ini
base_url=http://192.168.1.179:11434/v1
model=qwen3.6:35b
api_key=ollama
```

If `translate.env` is absent the feature is compiled out and the button hidden.
The request runs on a background thread, so the overlay stays responsive while a
large model thinks. QR detection and settings persist separately in
`~/.config/monado-frame/config.json`.

## Sharing (optional)

The Share button uploads to a [Picsur](https://github.com/CaramelFur/Picsur)
instance and copies the resulting link. Same build-time gating: copy
`picsur.env.example` to `picsur.env`, set your instance + API key, and rebuild.

```ini
base_url=https://picsur.example.com
api_key=your-picsur-api-key
```

The link is `<base_url>/i/<id>.png`. Absent `picsur.env` => button hidden.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `MONADO_SCREENSHOT_DIR` | `~/Pictures/Monado` | Folder watched for new screenshots. |
| `MONADO_FRAME_OPACITY` | `0.92` | Panel glass opacity (0–1). |
| `MONADO_FRAME_NO_ALPHA` | unset | Set to disable alpha blending (opaque rectangular panels) if glass looks wrong. |
| `MONADO_FRAME_NO_LASER` | unset | Set to disable the 3D laser pointer. |
| `MONADO_FRAME_WRIST_POS` | `-0.04,-0.01,0.07` | Watch position offset `x,y,z` (metres) from the left grip pose. |
| `MONADO_FRAME_WRIST_ROT` | `90,180,63` | Hand-locked orientation `yaw,pitch,roll` (degrees). |
| `MONADO_FRAME_WRIST_FOV` | `35` | Reveal half-angle (degrees): the card shows while its face points within this of your head. |

The wrist card follows the left controller's **grip** pose with a fixed
hand-locked orientation, and appears only when you turn your wrist so the card's
face points toward your head (within `MONADO_FRAME_WRIST_FOV`). Adjust
`MONADO_FRAME_WRIST_POS` to place it; to find the axes, run once with
`MONADO_FRAME_WRIST_POS=0,0,0` (card at the palm) and bump one axis by `0.1` to
see which way it moves.
