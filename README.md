# flowengine

**A native, audio-reactive GPU visualizer that renders straight to the screen — no browser, no compositor, no window server.**

Speak, and a Jarvis-style "AI core" answers: your voice's loudness, its peak, and its
frequency spectrum drive a glowing white core, concentric ripples, and a radial FFT ring.
Built live on a Raspberry Pi CM5 (VideoCore VII), scanning out directly to the HDMI
framebuffer via DRM/KMS.

> Born out of `compute5-oracle` on 2026-07-28, iterating from a WebGL sketch on a physical
> screen until it felt right. This repo is the open-source home for the code.

---

## Three renderers, one look

| Path | Stack | Runs on | Status |
|---|---|---|---|
| **`flowengine/`** | Rust · DRM/KMS + GBM + EGL + OpenGL ES 2.0 | Raspberry Pi (bare framebuffer, no X/Wayland) | ✅ ~60 fps on CM5 |
| **`flowengine/web/voicefield.html`** | WebGL2 + Web Audio | any browser (served locally) | ✅ prototype / reference |
| **`flowengine-wgpu/`** | Rust · wgpu 25 + winit 0.30 + WGSL | macOS (Metal), Linux (Vulkan), Windows (DX12) | 🚧 port scaffold |

All three share the same signal path — capture the mic → FFT → a handful of uniforms
(`level`, `peak`, `bass`, spectrum) → one fragment shader.

```
mic → cpal / Web Audio → ringbuf → realfft (Blackman, dB byte-map, critically-damped
      spring smoothing) → GPU: FFT texture + level/peak/bass uniforms → fragment shader
      → framebuffer / canvas
```

---

## The core, in words

- A **soft white "heaven" core** that holds white on a peak and fades out slowly (a
  fast-attack / slow-release peak-hold envelope — no throb, no flicker).
- **Concentric ripples** radiating outward — analytic wave physics with retarded time
  `t − r/c` and an echo rippling back inward.
- A **radial FFT ring** whose spiky bars fade in only on peaks, so the resting state stays
  calm instead of vibrating with every frame.
- A live dev **HUD** (level / peak / bass / peak-Hz / fps) drawn with an 8×8 bitmap font.

## Two hard-won lessons (baked into `flowengine/src/main.rs`)

1. **Pick the 8-bit EGL config, not the default.** On V3D the first config is 10-bit
   `XRGB2101010`; paired with an 8-bit `drmModeAddFB` scanout it renders as rainbow
   garbage. The code enumerates configs and picks native visual `0x34325258` (`XRGB8888`).
2. **Root can't reach the user's PipeWire.** DRM master needs `sudo`, which strips
   `XDG_RUNTIME_DIR` / `PULSE_SERVER`, so `cpal` fails before the first frame. `main()`
   reconstructs them in-process from `SUDO_UID`.

---

## Quick start

**Raspberry Pi (native framebuffer):**
```bash
sudo apt install -y libasound2-dev
cd flowengine && cargo build --release
sudo chvt 4
sudo ./target/release/flowengine /dev/dri/card1 0     # 0 = run forever
```

**Web prototype (any machine):**
```bash
cd flowengine/web && python3 -m http.server 8788
# open http://localhost:8788/voicefield.html  → "Tap to speak"
```

**macOS / cross-platform (wgpu):**
```bash
cd flowengine-wgpu && cargo run --release
# note: mic access requires launching from a Terminal that has Microphone permission
```

See each subdirectory's `README.md` for the details.

## License

MIT — see [LICENSE](LICENSE).
