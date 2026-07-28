#!/usr/bin/env bash
# Universal installer for flowengine — the native audio-reactive GPU visualizer.
#
#   curl -fsSL https://raw.githubusercontent.com/Soul-Brews-Studio/nat-build-with-oracle/main/install.sh | bash
#     …or, from a clone:  ./install.sh
#
# It detects the platform and builds the right renderer. It never enables a
# display-grabbing service without you asking — flowengine takes over a framebuffer
# and the mic, so enabling it is always an explicit opt-in step, printed at the end.
set -euo pipefail

REPO="https://github.com/Soul-Brews-Studio/nat-build-with-oracle"
say() { printf '\033[36m▸ %s\033[0m\n' "$*"; }

# --- locate the source (this repo) -------------------------------------------
if [ -f "$(dirname "${BASH_SOURCE[0]:-$0}")/flowengine/Cargo.toml" ]; then
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
else
  ROOT="${HOME}/.flowengine-src"
  say "cloning $REPO -> $ROOT"
  rm -rf "$ROOT"; git clone --depth 1 "$REPO" "$ROOT"
fi

command -v cargo >/dev/null 2>&1 || { echo "need Rust/cargo: https://rustup.rs"; exit 1; }

OS="$(uname -s)"
case "$OS" in
  Linux)
    if [ -e /dev/dri/card0 ] || [ -e /dev/dri/card1 ]; then
      say "Linux + DRM detected — building the native framebuffer renderer"
      command -v pkg-config >/dev/null && pkg-config --exists alsa 2>/dev/null || \
        say "tip: sudo apt install -y libasound2-dev  (cpal ALSA backend)"
      ( cd "$ROOT/flowengine" && cargo build --release )
      BIN="$ROOT/flowengine/target/release/flowengine"
      say "built: $BIN"
      cat <<EOF

  flowengine is built. It needs DRM master (a free VT) + the mic. To run once:
      sudo chvt 4 && sudo $BIN /dev/dri/card1 0

  To install as a boot service (OPT-IN — it will take over the console/HDMI):
      sudo cp $ROOT/flowengine/deploy/flowengine.service /etc/systemd/system/
      # edit ExecStart if your binary path differs, then:
      sudo systemctl daemon-reload && sudo systemctl enable --now flowengine
      # bars outward instead of inward:  Environment=FLOWENGINE_FLIP=1
EOF
    else
      say "Linux (no /dev/dri) — building the wgpu/Vulkan windowed renderer"
      ( cd "$ROOT/flowengine-wgpu" && cargo build --release )
      say "run:  $ROOT/flowengine-wgpu/target/release/flowengine"
    fi
    ;;
  Darwin)
    say "macOS detected — building the wgpu/Metal renderer as a signed .app"
    ( cd "$ROOT/flowengine-wgpu" && ./make-app.sh )
    say "done — click Allow on the microphone prompt, then speak. (Space toggles bars in/out)"
    ;;
  *)
    echo "unsupported OS: $OS (this installs on Linux or macOS)"; exit 1 ;;
esac
