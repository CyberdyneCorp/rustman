#!/usr/bin/env bash
# Render the README screenshots (docs/images/tui-*.svg) with the screenshot
# example: each frame is the sample workspace in this folder, an optional
# sample response and the keys pressed before the capture. No request is
# sent. Run from anywhere: crates/rustman-tui/examples/screenshots/render.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
out="$repo/docs/images"

cargo build --quiet --manifest-path "$repo/Cargo.toml" -p rustman-tui --example screenshot
shot="$repo/target/debug/examples/screenshot"

# A fixed, neutral path: the MCP trust dialog shows the working directory.
work=/tmp/rustman-screenshots
rm -rf "$work"
mkdir -p "$work"
cp -R "$here/workspace" "$work/petstore-api"
cd "$work"

# frame NAME [screenshot flags...]
frame() {
    local name="$1"
    shift
    "$shot" --ansi --size 136x30 "$@" petstore-api | python3 "$here/ansi2svg.py" > "$out/tui-$name.svg"
    echo "docs/images/tui-$name.svg"
}

pet=(--open pets/get-pet.rman.yaml --response "$here/pet.json")
frame shell "${pet[@]}" --keys "Tab 2 Tab"
frame timing "${pet[@]}" --keys "Tab 4 Tab Tab ] ] Shift+Tab"
frame assertions --open pets/update-pet.rman.yaml --response "$here/pet.json" --keys "Ctrl+B Tab 3 Tab Tab ] ] ]"
frame environments "${pet[@]}" --keys "Alt+E"
frame palette "${pet[@]}" --keys "Ctrl+P s w e n v"
frame help "${pet[@]}" --keys "F1"

gql=(--open graphql/pet-with-owner.rman.yaml)
frame graphql "${gql[@]}" --response "$here/graphql-pet.json" --keys "Ctrl+B Tab Tab Tab ]"
frame graphql-schema "${gql[@]}" --keys "Tab 3 Tab Down Down Enter"
frame jsonrpc --open rpc/get-balance.rman.yaml \
    --keys "Ctrl+B Tab Tab End Backspace Backspace Backspace Backspace Backspace Backspace Backspace"
grpc=(--open grpc/list-products.rman.yaml --response "$here/grpc-list-products.json")
frame grpc "${grpc[@]}" --keys "Ctrl+B"
frame grpc-timeline "${grpc[@]}" --keys "Ctrl+B Tab 2 Tab Tab ] ]"
frame mcp --keys "Alt+I Down Enter"

rm -rf "$work"
