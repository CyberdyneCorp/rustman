#!/usr/bin/env python3
"""Scaffold, build, test and add a plugin from every `rustman plugin new`
template (add-plugin-system task 6.2, "Scaffold builds").

For each extension point this runs, in a fresh workspace under
`target/plugin-template-check/`:

    rustman plugin new p-<point> --kind <point> --sdk <this repo's SDK>
    cargo build --target wasm32-wasip2 --release --locked
                                                   (against this repo's SDK)
    rustman plugin test ./p-<point>                (must exit 0)
    rustman plugin add ./p-<point>                 (must exit 0)

and finally `rustman plugin verify` (must exit 0). The workspace sits inside
this repository so the pinned toolchain of rust-toolchain.toml is used, and
every build shares the repository's target directory.

Builds are reproducible: each scaffold gets the committed lockfile
`scripts/plugin-template-locks/<point>.Cargo.lock` and builds with
`--locked`, so a new release of a template dependency cannot change (or
break) the job. After changing a template's dependencies, regenerate them:

    python3 scripts/check_plugin_templates.py --update-locks

Needs the wasm32-wasip2 target: `rustup target add wasm32-wasip2`.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

POINTS = ["auth", "assertion", "generator", "transformer", "importer", "kind", "report"]

REPO = Path(__file__).resolve().parent.parent
TARGET = REPO / "target"
SDK = REPO / "crates" / "rustman-plugin-sdk"
LOCKS = REPO / "scripts" / "plugin-template-locks"
EXE = ".exe" if os.name == "nt" else ""


def run(args, cwd, env=None):
    print("$ " + " ".join(str(a) for a in args), flush=True)
    result = subprocess.run(args, cwd=cwd, env=env)
    if result.returncode != 0:
        sys.exit(f"failed with exit code {result.returncode}: {' '.join(map(str, args))}")


def lock(project, point, env, update):
    """Give `project` the committed lockfile of `point` (or, with
    `update`, resolve one and commit it)."""
    committed = LOCKS / f"{point}.Cargo.lock"
    if update:
        run(["cargo", "generate-lockfile"], project, env)
        LOCKS.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(project / "Cargo.lock", committed)
        return
    if not committed.exists():
        sys.exit(f"missing {committed.relative_to(REPO)}: run with --update-locks")
    shutil.copyfile(committed, project / "Cargo.lock")


def main():
    update = "--update-locks" in sys.argv[1:]
    env = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))
    run(["cargo", "build", "-p", "rustman-cli", "--locked"], REPO, env)
    run(
        ["cargo", "clippy", "-p", "rustman-plugin-sdk", "--all-features",
         "--target", "wasm32-wasip2", "--locked", "--", "-D", "warnings"],
        REPO,
        env,
    )
    rustman = TARGET / "debug" / f"rustman{EXE}"
    check = TARGET / "plugin-template-check"
    shutil.rmtree(check, ignore_errors=True)
    check.mkdir(parents=True)
    run([rustman, "init", "ws", "--bare"], check)
    workspace = check / "ws"
    for point in POINTS:
        name = f"p-{point}"
        run([rustman, "plugin", "new", name, "--kind", point, "--sdk", SDK], workspace)
        lock(workspace / name, point, env, update)
        run(
            ["cargo", "build", "--target", "wasm32-wasip2", "--release", "--locked"],
            workspace / name,
            env,
        )
        built = TARGET / "wasm32-wasip2" / "release" / f"{name.replace('-', '_')}.wasm"
        component = workspace / name / "target" / "wasm32-wasip2" / "release" / built.name
        component.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(built, component)
        run([rustman, "plugin", "test", f"./{name}"], workspace)
        run([rustman, "plugin", "add", f"./{name}"], workspace)
    run([rustman, "plugin", "verify"], workspace)
    print(f"every template ({', '.join(POINTS)}) scaffolds, builds, passes its tests and is added")


if __name__ == "__main__":
    main()
