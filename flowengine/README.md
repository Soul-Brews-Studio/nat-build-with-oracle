# flowengine

A native, audio-reactive GPU visualizer for the Raspberry Pi (compute5.local, CM5) —
rendered **straight to the DRM/KMS framebuffer** via GBM + EGL + OpenGL ES 2.0. No X11,
no Wayland, no compositor, no browser. It captures the microphone, FFTs it, and draws an
"alien / AI" Jarvis-style core with a frequency-separated radial spectrum and a live dev HUD.

Built 2026-07-28 on compute5-oracle, iterating from a WebGL prototype (`voicefield.html`).

## Pipeline

```
Samson USB mic → cpal (ALSA "default" → pi PipeWire) → ringbuf
   → rustfft (Blackman, dB byte-map, CdsTween spring)  [src/dsp.rs]
   → glow: FFT→LUMINANCE texture + level/peak/bass uniforms
   → 2-pass render:  scene @ 720p (FBO)  →  upscale @ 1080p + crisp HUD text
   → DRM/KMS page-flip → HDMI display
```

- **src/audio.rs** — `cpal` capture, downmix→mono, lock-free SPSC ring.
- **src/dsp.rs** — `realfft` analysis: RMS level, bass band, 256-bin dB spectrum, KlakMath
  critically-damped-spring (`CdsTween`) smoothing.
- **src/text.rs** — `font8x8` bitmap-font rasterizer for the on-screen dev HUD.
- **src/main.rs** — DRM/GBM/EGL/GLES setup, the two shaders (scene + composite), the
  render loop, and a self-snapshot (`glReadPixels` → `/tmp/flowsnap.ppm`).

## The two hard-won lessons (baked into the code)

1. **8-bit config, not the default.** On V3D the first EGL config is 10-bit `XRGB2101010`;
   paired with an 8-bit `drmModeAddFB` scanout it renders as rainbow garbage. `main.rs`
   enumerates configs and picks native visual `0x34325258` (`XRGB8888`).
2. **Root can't reach the user PipeWire.** DRM master needs `sudo`, which strips
   `XDG_RUNTIME_DIR`/`PULSE_SERVER`, so `cpal` fails before the first frame. `main()`
   reconstructs them in-process from `SUDO_UID`.

## Build & run (on compute5.local)

```bash
sudo apt install -y libasound2-dev            # one-time: cpal ALSA backend
cd flowengine && cargo build --release

# precondition: pi-PipeWire must own the Samson mic (stop HA's hassio_audio if it grabbed
# the cards). verify: arecord -D default -f S16_LE -c1 -r48000 -d1 /dev/null   # rc 0

# run on a free VT (needs DRM master); sudo is required for KMS
sudo chvt 4
sudo ./target/release/flowengine /dev/dri/card1 0     # 0 = run forever
```

Args: `flowengine [dri-card] [duration-seconds]`. Renders 1280×720 → upscaled to the
connector's mode; HUD drawn at full res. ~60 fps on CM5 / VideoCore VII.
