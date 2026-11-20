#!/usr/bin/env bash
# Regenerate docs/images/demo.gif: build rustman, copy the demo workspace to a
# neutral temporary folder, store the demo token, start the todo backend,
# record demo.tape with VHS and stop the backend.
#
# Needs: cargo, python3, vhs (with ttyd and ffmpeg). Run from anywhere:
#   examples/demo/render.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

for tool in cargo python3 vhs; do
    command -v "$tool" >/dev/null || { echo "render.sh: $tool is not installed" >&2; exit 1; }
done

cargo build --manifest-path "$repo/Cargo.toml" -p rustman-cli --release --locked
export PATH="$repo/target/release:$PATH"

# A fresh copy with no history, in a path that shows nothing personal. The
# token goes to the file store (.rustman/secrets.yaml), never the keychain.
export DEMO_DIR="${TMPDIR:-/tmp}/rustman-demo/todo-api"
export RUSTMAN_SECRET_STORE=file
rm -rf "$DEMO_DIR"
mkdir -p "$(dirname "$DEMO_DIR")"
cp -R "$here/workspace" "$DEMO_DIR"
(cd "$DEMO_DIR" && printf '%s' demo-token | rustman secret set api_token --env local)

# The TUI reads tui.toml from the config folder: use the defaults.
export XDG_CONFIG_HOME="$DEMO_DIR/.rustman/config"

python3 "$here/backend.py" &
backend=$!
trap 'kill "$backend" 2>/dev/null || true' EXIT
for _ in $(seq 50); do
    curl -fsS http://127.0.0.1:8765/health >/dev/null 2>&1 && break
    sleep 0.1
done

cd "$repo"
vhs "$here/demo.tape"
ls -lh docs/images/demo.gif
