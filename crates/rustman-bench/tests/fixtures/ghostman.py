"""A raw HTTP/1.1 server reproducing the old Ghostman wire behavior.

The stress regression corpus (add-stress-testing task 4.7) checks that
`rustman stress --wire` flags each of these:

- a request with both Content-Length and Transfer-Encoding is answered 200;
- a garbage request line is answered 200;
- a header block over 64 KiB drops the connection without a response.

It listens on 127.0.0.1, prints its port on the first line of stdout, and
serves until killed. Standard library only.
"""

import socket
import socketserver
import sys

MAX_HEAD = 64 * 1024


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        conn = self.request
        conn.settimeout(10)
        head = b""
        try:
            while b"\r\n\r\n" not in head:
                if len(head) > MAX_HEAD:
                    # Ghostman dropped the connection on large headers.
                    return
                chunk = conn.recv(8192)
                if not chunk:
                    return
                head += chunk
        except (socket.timeout, OSError):
            return
        if head.index(b"\r\n\r\n") + 4 > MAX_HEAD:
            return
        # Ghostman answered 200 whatever the request line or framing.
        reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
        try:
            conn.sendall(reply)
        except OSError:
            pass


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    with Server(("127.0.0.1", 0), Handler) as server:
        print(server.server_address[1], flush=True)
        server.serve_forever()


if __name__ == "__main__":
    sys.exit(main())
