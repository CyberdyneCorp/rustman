#!/usr/bin/env python3
"""A tiny in-memory todo API for the rustman demo. Standard library only.

    python3 examples/demo/backend.py            # listens on 127.0.0.1:8765

Endpoints:
    GET    /health
    GET    /todos
    POST   /todos          {"title": "..."} -> 201 with the new todo
    GET    /todos/{id}
    DELETE /todos/{id}

Every /todos endpoint needs `Authorization: Bearer demo-token`; a missing or
wrong token gets 401. Ids are sequential and the data resets on restart.
"""

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
PORT = int(os.environ.get("DEMO_PORT", "8765"))
TOKEN = os.environ.get("DEMO_TOKEN", "demo-token")

SEED = ["Write the API spec", "Add request assertions", "Record the demo"]


class Store:
    def __init__(self):
        self.todos = {}
        self.next_id = 1
        for title in SEED:
            self.add(title)
        self.todos[1]["done"] = True

    def add(self, title):
        todo = {"id": self.next_id, "title": title, "done": False}
        self.todos[self.next_id] = todo
        self.next_id += 1
        return todo


STORE = Store()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "todo-demo/1.0"
    sys_version = ""

    def log_message(self, format, *args):  # no access log noise
        pass

    def send_json(self, status, payload):
        body = json.dumps(payload, indent=2).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authorized(self):
        if self.headers.get("Authorization") == f"Bearer {TOKEN}":
            return True
        self.send_json(401, {"error": "unauthorized"})
        return False

    def todo_id(self):
        """The id of /todos/{id}, or None when the path is not one."""
        parts = self.path.split("?")[0].strip("/").split("/")
        if len(parts) == 2 and parts[0] == "todos" and parts[1].isdigit():
            return int(parts[1])
        return None

    def path_only(self):
        return self.path.split("?")[0].rstrip("/") or "/"

    def do_GET(self):
        path = self.path_only()
        if path == "/health":
            return self.send_json(200, {"status": "ok", "todos": len(STORE.todos)})
        if path == "/todos":
            if self.authorized():
                self.send_json(200, list(STORE.todos.values()))
            return
        todo_id = self.todo_id()
        if todo_id is None:
            return self.send_json(404, {"error": "not found"})
        if self.authorized():
            todo = STORE.todos.get(todo_id)
            if todo is None:
                return self.send_json(404, {"error": f"todo {todo_id} not found"})
            self.send_json(200, todo)

    def do_POST(self):
        # Read the body first so the kept-alive connection stays in sync.
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if self.path_only() != "/todos":
            return self.send_json(404, {"error": "not found"})
        if not self.authorized():
            return
        try:
            payload = json.loads(raw or b"{}")
        except ValueError:
            return self.send_json(400, {"error": "body must be JSON"})
        title = payload.get("title") if isinstance(payload, dict) else None
        if not isinstance(title, str) or not title.strip():
            return self.send_json(400, {"error": "title is required"})
        self.send_json(201, STORE.add(title.strip()))

    def do_DELETE(self):
        todo_id = self.todo_id()
        if todo_id is None:
            return self.send_json(404, {"error": "not found"})
        if not self.authorized():
            return
        if STORE.todos.pop(todo_id, None) is None:
            return self.send_json(404, {"error": f"todo {todo_id} not found"})
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()


def main():
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
