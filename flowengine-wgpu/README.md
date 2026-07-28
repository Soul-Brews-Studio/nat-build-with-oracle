# flowengine-wgpu

The cross-platform port of **flowengine** — **wgpu 25 + winit 0.30 + WGSL**, running on
macOS (Metal), Linux (Vulkan) and Windows (DX12). Same signal path and look as the Pi
native build: a swirling **"Starry Night" (Van Gogh) porthole** with a living golden rim
and an FFT **voiceprint** that reacts to your voice.

## Run

```bash
cargo run --release
```

### macOS microphone (important)

macOS gates the mic behind TCC (privacy). A bare `cargo run` binary is attributed to the
launching Terminal; without an `Info.plist` declaring `NSMicrophoneUsageDescription`,
macOS silently feeds **silence (all-zero samples)** — the visual runs but never reacts,
and there is no error. Build and launch the signed `.app` instead so the permission
prompt appears:

```bash
./make-app.sh          # builds & signs dist/flowengine.app, then opens it
# → click "Allow" on the microphone prompt, then speak
```

## Controls

| Key | Action |
|-----|--------|
| **Space** | toggle the FFT bars **inward ⟷ outward** |
| **Esc** | quit |

The window title is a live readout: `LVL / PEAK / Hz / VOICE% / [VOICE|noise|quiet]`.

## The visual

- A **golden porthole** — inside is a swirling ultramarine/gold Van Gogh sky (a cheap
  3-iteration sin domain-warp + brushstroke texture); outside the circle is dimmed to a
  faint filtered backdrop. The gold rim **breathes**, a shimmer courses around it, and a
  glint travels the ring.
- The **FFT voiceprint** bars hang from the rim (in or out), colour-shifting cyan→gold.
- A central **golden star** with a peak-hold bloom.
- Everything reacts to a **voice-activity envelope** (`u_voice`), so it swells for human
  speech and stays calm for noise/silence.

Metal note: `pow(x, n)` is undefined for `x < 0` on Metal, so every squared term is
written `x*x` and `pow()` is only used on provably non-negative bases.
