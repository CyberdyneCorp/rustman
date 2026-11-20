# README demo

The animated GIF at the top of the README, `docs/images/demo.gif`, is a real session recorded with [VHS](https://github.com/charmbracelet/vhs). This folder has everything it needs:

| File | What it is |
|---|---|
| `backend.py` | A tiny todo API (Python 3 standard library only) on `127.0.0.1:8765`: `GET /health`, `GET /todos`, `POST /todos`, `GET /todos/{id}`, `DELETE /todos/{id}`. The `/todos` endpoints need `Authorization: Bearer demo-token` and answer `401` otherwise. Data is in memory, ids are sequential and three todos are seeded. |
| `workspace/` | The Rustman workspace: a `local` environment with `base_url` and the `api_token` secret, `health`, and a `todos` collection (`list-todos`, `create-todo` capturing `id`, `get-todo` using `{{id}}`) with status, JSONPath and latency assertions. |
| `demo.tape` | The VHS script. |
| `render.sh` | Builds `rustman` (release), copies the workspace to a temporary folder, stores the token with `rustman secret set` in the file store, starts the backend, runs `vhs demo.tape` and stops the backend. |

Regenerate the GIF (needs `cargo`, `python3` and `vhs` with its `ttyd` and `ffmpeg`):

```bash
examples/demo/render.sh
```

To try the workspace by hand:

```bash
python3 examples/demo/backend.py &
cd examples/demo/workspace
printf '%s' demo-token | RUSTMAN_SECRET_STORE=file rustman secret set api_token --env local
RUSTMAN_SECRET_STORE=file rustman run --env local
```

The recording shows: the TUI opening on the workspace, the environment switcher from the command palette (`Ctrl+P`, `swenv`) with the token masked, `list-todos` sent (`Ctrl+R`) and its JSON tree, `create-todo` sent with its `201`, the captured `id`, the Timing and Assertions tabs (`]`), quitting (`Ctrl+Q`), and `rustman run --env local` passing all four requests.
