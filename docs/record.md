# Traffic recorder

`rustman record` runs a local recording proxy. Point an app or a CLI at it and every request it sends is relayed to the real server and recorded, so you can see what the app actually sends. HTTPS stays opaque unless you explicitly include its host and trust a local certificate authority (CA) that Rustman creates for the workspace.

The behavior is specified in [`openspec/specs/traffic-recorder`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/traffic-recorder/spec.md).

## Starting the proxy

```text
$ rustman record
Recording proxy on http://127.0.0.1:8888
export HTTP_PROXY=http://127.0.0.1:8888
export HTTPS_PROXY=http://127.0.0.1:8888
GET http://api.example.test/v1/items 200 14 ms 512 B
TUNNEL www.google.com:443 1834 B up, 5120 B down
```

Run the printed `export` lines in another shell (or set the proxy in the app's settings) to route traffic through the proxy. They are POSIX shell syntax; on Windows use `$env:HTTP_PROXY = "http://127.0.0.1:8888"` and `$env:HTTPS_PROXY = "http://127.0.0.1:8888"` in PowerShell, or `set HTTP_PROXY=http://127.0.0.1:8888` and `set HTTPS_PROXY=http://127.0.0.1:8888` in `cmd`. Rustman never changes system or browser proxy settings itself. Press Ctrl+C to stop recording; the command then exits with code 0.

| Flag | Effect |
|------|--------|
| `--host <HOST>` | Address to bind: an IP address or `localhost` (default: `[record] host` in `rustman.toml`, else `127.0.0.1`). |
| `--port <PORT>` | Port to bind (default: `[record] port`, else `8888`); `0` picks a free port and the banner shows it. |
| `--allow-remote` | Required to bind an address reachable from the network, such as `0.0.0.0`. A warning is printed: anyone who can reach the port can use the proxy and see intercepted traffic. |
| `--intercept <GLOB>` | Decrypt HTTPS to hosts matching the glob (repeatable, case-insensitive, `*.example.com`). Needs the workspace's [local CA](#the-local-ca). Other hosts stay [opaque tunnels](#tunnels-and-interception). |
| `--workspace <DIR>` | The workspace whose `[record]` settings and local CA are used. Without it the workspace is searched from the current directory upwards; recording without interception works outside a workspace. |
| `--include-host`, `--exclude-host`, `--method`, `--content-type`, `--keep-static` | Choose what is recorded; see [Filters and limits](#filters-and-limits). Everything is still proxied. |
| `--json` | Print one redacted JSON object per capture; see [JSON lines](#json-lines). |
| `--upstream <URL>`, `--insecure` | [Reverse-proxy mode](#reverse-proxy-mode) for clients that cannot use a proxy. |
| `--save`, `--into <DIR>`, `--env <NAME>`, `--store-secrets` | Save the captures as request files when recording stops; see [Saving captures as request files](#saving-captures-as-request-files). |

The defaults can live in the workspace:

```toml
[record]
host = "127.0.0.1"   # the flags win over these
port = 8888
intercept = ["*.example.com"]
max_body = 10485760          # bytes of each body kept in a capture (10 MiB)
max_captures = 2000          # captures kept, oldest evicted first
max_buffer_bytes = 536870912 # bytes all kept bodies may take (512 MiB)
```

A port that cannot be bound, or a network address without `--allow-remote`, stops the command with exit code 2:

```text
$ rustman record
error: record: cannot bind 127.0.0.1:8888: address in use
$ rustman record --host 0.0.0.0
error: record: binding a non-loopback address requires --allow-remote
```

### What is printed

One line per capture, as it completes, with the method, absolute URL, status, duration and response size (`truncated` when only part of a body was kept). Lines are redacted: the credentials carried by `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie`, and every value that [saving](#saving-captures-as-request-files) would turn into a placeholder (the values of `X-Api-Key`-style headers and of query and body fields named like `token`, `password`, `secret` or `api_key`, values starting with an auth scheme such as `Bearer …` in any header or field, and known token formats such as `ghp_…` or `AKIA…` wherever they appear, under any name) are replaced by `«redacted:<name>»` wherever they reappear, URLs included. A request the upstream could not answer shows `---` and the error, and the client gets `502 Bad Gateway` (`504 Gateway Timeout` when no response head arrived in time, see [Timeouts](#timeouts)). Lines are printed by a thread of their own, so a slow terminal or pipe never slows the proxy down: when more than 1024 lines wait, the newest are dropped and stderr shows `warning: <n> captures not shown (output too slow)` (they are still captured, saved and exported). When recording stops, the lines still waiting are printed (for up to 2 seconds) before the `--har` and `--save` output.

### What the proxy accepts

- Plain HTTP proxy requests (`GET http://host/path HTTP/1.1`): forwarded to the origin, the response streamed back byte for byte (bodies are not decoded), and the exchange recorded.
- `CONNECT host:port` tunnels: see below.
- Anything else (an origin-form request such as `GET /v1/users`) gets `400`: the proxy is not a web server. Use [reverse-proxy mode](#reverse-proxy-mode) for clients that send those.
- A URL with credentials in it (`http://user:password@host/`) gets `400` and is not recorded: the password would otherwise reach the upstream `Host` header and the captures. Send credentials in an `Authorization` header.

The upstream leg uses the same connector as `rustman send`: DNS, a 10-second connect timeout and TLS verification with the OS trust store plus the workspace's `[http] ca_bundle` (see [Upstream TLS](#upstream-tls)). Hop-by-hop headers (`Connection`, `Proxy-Connection`, `Proxy-Authorization`, `Keep-Alive`, `TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`) are not forwarded. Each stored body keeps at most its first 10 MiB; the client always receives the whole body.

### Filters and limits

Filters decide what is **recorded**; every request is still proxied, so the app keeps working.

| Flag | Records only |
|------|--------------|
| `--include-host <GLOB>` | Hosts matching the glob (repeatable, case-insensitive, `*.example.com`; no port). |
| `--exclude-host <GLOB>` | Hosts not matching the glob (repeatable); wins over `--include-host`. |
| `--method <METHOD>` | This method (repeatable, case-insensitive). |
| `--content-type <PREFIX>` | Exchanges whose request or response `Content-Type` starts with the prefix (repeatable, `application/json`). |

Host filters also apply to tunnel and `TLS REJECTED` entries. Static assets are dropped by default: responses whose type is an image, font, audio, video, CSS or JavaScript, and URLs ending in an image, font, media, `.css`, `.js`, `.mjs` or `.map` extension. `--keep-static` records them too.

Memory stays bounded: each body keeps its first `max_body` bytes (10 MiB by default) and the capture is marked `truncated`, while the client still receives the whole body. The capture list keeps at most `max_captures` captures (2000) whose bodies take at most `max_buffer_bytes` (512 MiB) together, evicting the oldest first. The copies of the bodies still being received (many connections, HTTP/2 streams) share a budget of the same `max_buffer_bytes`: once it is used up, a body being copied stops growing and its capture is marked `truncated`, while the traffic still flows in full. All three can be set in `[record]`.

### Timeouts

| What | Default | `[record]` key |
|------|---------|----------------|
| Upstream connection (DNS, TCP and TLS) | 10 s | |
| A client's request head, and the TLS handshake of an intercepted tunnel | 30 s | |
| The upstream's response head, counted once the request body is sent (a long upload is not cut short) | 60 s | `response_timeout_ms` |
| A client connection (plain HTTP, an opaque tunnel, or an intercepted HTTP/1.1 or HTTP/2 connection) without a byte either way, a request body that stalls included | 5 min | `idle_timeout_ms` |

A response head that does not arrive in time gets `504 Gateway Timeout` and is recorded with the error `no response within 60 s`. An idle tunnel is closed and recorded with `idle timeout: no traffic for 300 s`; an idle plain or intercepted connection is closed (clients open a new one), so a client that announces a body (`Content-Length` or chunked) and stops sending it frees its connection slot. Set a key to a number of milliseconds (`idle_timeout_ms = 900000` for long-polling apps); `0` keeps the default.

`rustman.toml` is committed, so a cloned workspace can carry `[record]` settings you did not choose. `rustman record` prints a warning on stderr when `[record] intercept` decides which hosts are decrypted (no `--intercept` flag was given), and when `max_body`, `max_captures` or `max_buffer_bytes` is above its default. The TUI recorder pane shows the interception scope in its header.

### JSON lines

`rustman record --json` prints one JSON object per capture on stdout, and the banner, the `--har` line and the `--save` summary on stderr, so stdout can be piped into `jq`:

```json
{"type":"exchange","method":"GET","url":"http://api.example.test/v1/me","version":"HTTP/1.1","status":200,"duration_ms":12,"intercepted":false,"request_headers":[{"name":"authorization","value":"«redacted:Authorization»"}],"response_headers":[{"name":"content-type","value":"application/json"}],"request_size":0,"response_size":512,"truncated":false,"error":null,"id":1,"ts_ms":1700000000123}
```

Tunnels have `"type":"tunnel"` with `host`, `port`, `bytes_up`, `bytes_down` and `duration_ms`; rejected handshakes have `"type":"tls_rejected"` with `host`, `port` and `hint`. Objects are redacted like the summary lines: `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie` values are replaced whole, and their credentials, like the values of secret-named headers, query parameters and body fields, are redacted wherever else they appear. Bodies are not printed; their sizes are.

## Tunnels and interception

For every `CONNECT`, the host is matched against the `--intercept` globs **before** any TLS handshake. A `CONNECT` host that is neither a DNS name nor an IP address (such as `*.example.com`) gets `400 Bad Request`, so no certificate is ever minted for a wildcard:

- **Not included** (the default for every host): the tunnel is relayed byte for byte, nothing is decrypted, and the capture list shows only `TUNNEL host:port` with the bytes sent each way. Banking or personal traffic in the same browser stays opaque while you record.
- **Included**: the proxy completes the TLS handshake itself with a certificate for that host signed by the local CA, decrypts HTTP/1.1 or HTTP/2 (negotiated by ALPN), forwards each request to that same host over a new verified TLS connection, and records the full exchange. Requests always go to the `CONNECT` host, whatever their `Host` header says.

A client that does not trust the local CA, or pins its certificates, aborts the handshake. The capture list then shows:

```text
TLS REJECTED api.example.com: client rejected the Rustman CA (not installed or pinned); exclude the host or install the CA
```

Remove the host from `--intercept` to let such an app work again (its traffic then becomes a tunnel), or [install the CA](#trusting-the-ca).

## Reverse-proxy mode

Some clients cannot be pointed at a proxy. `--upstream <URL>` turns the recorder into a reverse proxy: it accepts plain requests on the listen address and forwards each one to that origin, with `Host` rewritten to it.

```text
$ rustman record --upstream https://api.example.com --port 9000
Recording reverse proxy on http://127.0.0.1:9000 -> https://api.example.com
GET https://api.example.com/v1/users 200 85 ms 2048 B
```

Point the client at `http://127.0.0.1:9000` instead of `https://api.example.com`: `GET http://127.0.0.1:9000/v1/users` goes to `https://api.example.com/v1/users` and is recorded exactly like forward mode (filters, limits, `--json`, `--save`). Every request goes to the upstream, whatever its `Host` or absolute URL says; `CONNECT` gets `405`. The URL must be an origin (`http` or `https`, a host and an optional port, no path or user info).

HTTPS upstreams are verified like intercepted hosts (see [Upstream TLS](#upstream-tls)), so a self-signed upstream fails with `502 Bad Gateway`. `--insecure` (allowed only with `--upstream`, for that one host) turns verification off and prints a warning.

`--upstream` cannot be the proxy's own address (`--port 9000 --upstream http://127.0.0.1:9000`, or any other loopback spelling of it: `127.0.0.0/8`, `::1`, `::ffff:127.0.0.1`, `localhost`, `*.localhost`): every request would be forwarded back to the proxy, so the command exits with code 2: `record: --upstream "http://127.0.0.1:9000" is this proxy's own address; requests would loop`.

### Upstream TLS

Upstream servers, intercepted or `--upstream`, are verified with the OS trust store plus the CAs of `[http] ca_bundle` in `rustman.toml` (a PEM file, relative to the workspace root), the same rules as `rustman send`. Point `ca_bundle` at the private CA of a local or staging API to record it:

```toml
[http]
ca_bundle = "certs/dev-ca.pem"
```

A `ca_bundle` that cannot be read or holds no certificate stops the command with exit code 2. `[defaults] verify_tls = false` is deliberately not applied to the recorder: it would turn verification off for every intercepted host. Only `--insecure` turns it off, and only for the one `--upstream` host. Client certificates (`[http] client_cert`) are not presented to upstreams.

## The local CA

Interception needs a certificate authority that the client trusts. Rustman creates one per workspace, only when you ask:

```text
$ rustman record ca init
Created the local CA of this workspace
  subject: CN=Rustman Local CA (0b5c0b9e), O=Rustman
  SHA-256: 3F:9A:...:C2
  expires: 2027-11-21
  key:     OS keychain (rustman / record-ca/0b5c0b9e-58c4-4c1e-9f0e-6f1f4b7f2a10)
  cert:    /home/me/.config/rustman/record-ca/0b5c0b9e-58c4-4c1e-9f0e-6f1f4b7f2a10.pem
It is not trusted yet: run 'rustman record ca install' to trust it on this machine.
```

`rustman record --intercept ...` without a CA asks to create one when run in a terminal (type `yes`), and otherwise exits with code 2.

| Command | Effect |
|---------|--------|
| `rustman record ca init` | Create the CA. Refused (exit 2) when the workspace already has one. |
| `rustman record ca export [--out FILE]` | Print the certificate in PEM (or write it to `FILE`) and its SHA-256 fingerprint. Never the key. |
| `rustman record ca install [--yes]` | Trust the CA in this machine's trust store after printing what will be trusted and asking you to type `yes`. Answering anything else changes nothing and exits with code 1. |
| `rustman record ca uninstall` | Remove the CA from every trust store Rustman installed it in. |
| `rustman record ca rotate [--yes]` | Uninstall the old CA, delete its key and certificate, create a new one and print both fingerprints. The new CA is not installed. |
| `rustman record ca remove [--yes]` | Delete the CA when you are done with interception: remove it from the trust stores Rustman installed it in (the commands are printed), then delete its key from the keychain and its certificate, after you type `yes`. Answering anything else changes nothing and exits with code 1. A failed uninstall command keeps the CA, so it can be retried. |

Every CA command takes `--workspace <DIR>`.

### Security model

A trusted CA is a long-lived interception capability: whoever holds its private key can impersonate any website to this machine. Rustman limits that risk as follows.

- **Created only on request.** `rustman record ca init`, or the explicit prompt of `--intercept`. Nothing else creates a CA.
- **Key in the OS keychain only.** The private key is stored in the macOS Keychain, the Windows Credential Manager or the Linux Secret Service (service `rustman`, account `record-ca/<workspace-id>`). It is never written to a file, inside the workspace or anywhere else. Without a keychain (or with `RUSTMAN_SECRET_STORE=file`) there is no CA, and `ca init` fails with exit code 2: `record: a keychain is required to store the CA private key`. (Rustman's own integration tests use a stand-in keychain directory; that hook exists only in test builds, compiled in by the `record-test-hooks` feature, so `cargo build`, `cargo run` and `cargo install`, debug or release, have no such fallback.)
- **Outside the workspace.** The public certificate lives in the user config directory (`$XDG_CONFIG_HOME/rustman/record-ca/` or `~/.config/rustman/record-ca/`, `%APPDATA%\rustman\record-ca\` on Windows), so it cannot be committed by accident. The only workspace file involved is the per-clone id `.rustman/workspace-id` (git-ignored), created on first use like for secrets.
- **Narrow certificate.** ECDSA P-256, `CA:true` with `pathlen:0` (it can sign only end-entity certificates), key usage `keyCertSign`, extended key usage `serverAuth` (TLS server authentication only), valid for one year. Name constraints are not set: clients handle them inconsistently, and the intercept globs change between sessions. An installed CA can therefore vouch for any host while its key exists, and the recorder holds the key in memory while it runs: keep `--intercept` to the hosts you need (never `'*'` on a machine you browse with), and uninstall or rotate the CA when you are done.
- **Short-lived leaves, in memory.** Certificates for intercepted hosts are minted on demand, only for a DNS name or an IP address (never a wildcard), valid for 7 days, cached in memory for the session (up to 1000 hosts) and never written to disk. A rotated CA's leaves are gone with the session that minted them.
- **Never trusted automatically.** Only `rustman record ca install` changes a trust store, after showing the subject, SHA-256 fingerprint and expiry and asking you to type `yes` (or `--yes`). The exact commands are printed before they run.
- **Undo is one command.** Rustman records which stores it touched in `<config dir>/rustman/record-ca/<workspace-id>.installed`, so `uninstall` and `rotate` undo exactly that.
- **Scope first.** Only hosts you list with `--intercept` are decrypted; see [Tunnels and interception](#tunnels-and-interception).
- **Loopback only by default.** Binding another address needs `--allow-remote` and prints a warning. Clients that are not on loopback can then reach only globally routable addresses through the proxy: everything else is refused by default, namely `localhost`, loopback (`127.0.0.0/8`, `::1`), unspecified, private (RFC 1918 `10/8`, `172.16/12`, `192.168/16`), shared CGNAT `100.64.0.0/10` (with the metadata service `100.100.100.200`), link-local (`169.254.0.0/16` with cloud metadata, `fe80::/10`), unique local `fc00::/7` (with `fd00:ec2::254`), `192.0.0.0/24`, benchmarking `198.18.0.0/15`, documentation ranges, multicast, `240.0.0.0/4`, broadcast, every IPv6 address outside global unicast `2000::/3` and, inside it, `2001::/23` (Teredo and IETF assignments), `2001:db8::/32` and `3fff::/20`, and IPv6 addresses wrapping such an IPv4 address (IPv4-mapped and IPv4-compatible, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`). These are the same rules as the [plugin network](plugins.md#the-sandbox) (one classifier, `rustman_http::address::is_public_ip`). Literal addresses get `403` at once; names and other spellings (`127.1`, `2130706433`, a DNS name pointing at `10.0.0.5` or `169.254.169.254`) are checked after DNS, for tunnels, forwarded plain requests and requests inside intercepted tunnels alike, and refused with `502`. The check is made on the resolved addresses that are then connected to, so a name cannot resolve to a public address for the check and to a private one for the connection (DNS rebinding). Clients on loopback (this machine) may record local and private targets as before, and reverse-proxy mode is not checked: its upstream is your own choice.
- **Bounded connections.** At most 512 client connections are served at once (an open tunnel counts until it closes; more wait), and a client has 30 seconds to send each request head (inside intercepted tunnels too) or to finish the TLS handshake of an intercepted tunnel. An intercepted HTTP/2 connection carries at most 100 requests at once, the copies of bodies in flight share the `max_buffer_bytes` budget, and idle tunnels and connections, plain ones with a stalled request body included, are closed after 5 minutes (see [Timeouts](#timeouts)), so memory and connection slots stay bounded.
- **Upstream TLS is verified.** The proxy checks upstream certificates with the same rules as `rustman send` (see [Upstream TLS](#upstream-tls)); turning verification off is possible only for one `--upstream` host.

Run `rustman record ca uninstall` when you are done recording (or `rustman record ca remove` to delete the CA as well), and `rustman record ca rotate` if you suspect the key was exposed.

### Trusting the CA

`rustman record ca install` uses the platform tool:

| System | Install | Uninstall |
|--------|---------|-----------|
| macOS | `security add-trusted-cert -r trustRoot -p ssl <cert>` (your keychain; macOS may ask for your password) | `security remove-trusted-cert <cert>`, then `security delete-certificate -Z <SHA-1>` |
| Windows | `certutil -user -addstore Root <cert>` (current user, no administrator rights) | `certutil -user -delstore Root <SHA-1>` |
| Linux with `update-ca-certificates` (Debian, Ubuntu, Alpine) | `sudo install -m 644 <cert> /usr/local/share/ca-certificates/rustman-record-<id>.crt`, then `sudo update-ca-certificates` | `sudo rm -f /usr/local/share/ca-certificates/rustman-record-<id>.crt`, then `sudo update-ca-certificates --fresh` |
| Linux with p11-kit (Fedora, Arch) | `sudo trust anchor --store <cert>` | `sudo trust anchor --remove <cert>` |

On other systems `ca install` exits with code 2 and names the certificate file to trust by hand.

Some clients keep their own trust store:

- **Firefox** (its NSS store is not managed by Rustman): open Settings, Privacy & Security, Certificates, View Certificates, Authorities, Import, select the file written by `rustman record ca export --out rustman-ca.pem`, and tick "Trust this CA to identify websites". Remove it from the same dialog when you are done. Alternatively set `security.enterprise_roots.enabled` to `true` in `about:config` to use the OS store.
- **Node.js**: `export NODE_EXTRA_CA_CERTS=rustman-ca.pem`.
- **Python requests**: `export REQUESTS_CA_BUNDLE=rustman-ca.pem` (this replaces the default bundle for that shell).
- **curl**: `curl --cacert rustman-ca.pem ...`.

## Saving captures as request files

`--save` writes the captures as request files when you stop recording with Ctrl+C. It needs a workspace, and the flags are checked before the proxy starts.

```text
$ rustman record --save --env staging
Recording proxy on http://127.0.0.1:8888
...
GET https://api.example.com/users/17 200 31 ms 120 B
GET https://api.example.com/users/42 200 28 ms 118 B
^C
Saved 1 request file (0 unchanged), environment 'staging'
Secrets parameterized (values not stored; use --store-secrets or 'rustman secret set <name> --env staging'): auth_token
```

A session that recorded no request (nothing at all, or only tunnels and rejected handshakes) writes nothing, not even the environment, and prints `No requests recorded; nothing saved` instead of a summary. The TUI's `w` says the same for marked captures without a request.

Saving goes through the same pipeline as [`rustman import`](import.md):

1. **Paths are templatized.** Segments that look like identifiers (integers, UUIDs, 24-hex ObjectIds, ULIDs) become variables named after the previous segment in singular form: `/users/42` becomes `/users/{{user_id}}`, `/orders/9b2c…/items` becomes `/orders/{{order_id}}/items`. Without a usable previous segment the name is `id_1`, `id_2`... The first value seen is the variable's default in the environment.
2. **Captures are deduplicated.** One file per method and templated path, named after them (`GET /users/17` and `GET /users/42` give `get-users-id.rman.yaml`), with the most recent capture as the example (headers, query, body). Transport and `Proxy-*` headers are left out; a truncated or binary request body is not saved, with a warning. A request that got no response (the upstream could not be reached) is still saved, as a starting point, with a warning naming it.
3. **The base URL goes to the environment.** `--env <NAME>` (default `recorded`) gets `base_url` (scheme, host and port), so URLs read `{{base_url}}/users/{{user_id}}`. Another origin in the same save gets `base_url_2`, `base_url_3`... and a warning. An origin the environment already holds under one of these names keeps it, and a new origin never takes a name the environment uses for another value, so a later session over part of the same traffic writes the same URLs.
4. **Secrets become placeholders.** `Authorization: Bearer eyJ…` is saved as `Authorization: Bearer {{auth_token}}`, `Cookie` as `{{cookie}}`, and `X-Api-Key`-style headers, query and body fields named like `token`, `password`, `secret` or `api_key`, values starting with an auth scheme (`Bearer …`) in any header or body field, and known token formats (`ghp_…`, `AKIA…`, JWTs...) under any name get placeholders named after them. Each name is declared under `secrets:` in the environment. A name the environment already declares as a secret is reused for the same field in a later session (the value is not compared, since it may be stored nowhere; with `--store-secrets` the recorded value replaces the stored one); a plain variable of the same name is left alone and the secret gets a suffixed name (`api_key_2`). The raw values are written nowhere: with `--store-secrets` they are stored in the [secret store](environments.md) (the OS keychain), otherwise set them with `rustman secret set <name> --env <NAME>`. Long random-looking header and query values that none of these rules catch are kept but reported with a warning (the value is never printed); a value that did become a placeholder is never reported as kept.
5. **Files are written deterministically.** Files go to `--into <DIR>` (inside the workspace), or by default to `recorded/<host>/` with the host written as a file name (`api.example.com` becomes `recorded/api-example-com/`, `localhost:8080` becomes `recorded/localhost-8080/`). Saving the same captures again, in the same or a later session, changes nothing; a file that differs is overwritten with the recorded version, with a warning, so review the diff before committing. A `_collection.yaml` keeps the `order:` entries of earlier saves whose files are still there, ahead of the new ones, so saving part of the traffic again drops nothing.

Saving never writes a raw secret, but review the saved files before committing them: secret detection works on names and known formats, and the warnings point at values it was unsure about.

## HAR export

`--har <FILE>` writes every capture still in the buffer to `FILE` as [HAR 1.2](http://www.softwareishard.com/blog/har-12-spec/) when you stop recording with Ctrl+C, before `--save` runs. The path is relative to the current directory, and missing folders are created. The file is readable and writable by you only, redacted or not: mode `0600` on Unix (an existing file included), an access list for your user alone on Windows.

```text
$ rustman record --intercept "*.example.com" --har session.har
Recording proxy on http://127.0.0.1:8888
...
^C
Wrote 3 captures to /home/me/app/session.har
```

What the file holds:

- **One entry per capture**, oldest first: the request (method, URL, HTTP version, headers, query, body) and the response (status, headers, body), the start time and the duration as `wait`. Bodies are the stored part only; text bodies are written as text, others base64-encoded (`"encoding": "base64"` in `content`, a comment in `postData`). `cookies` arrays are empty: the `Cookie` and `Set-Cookie` headers carry them.
- **Secrets redacted.** `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie` values become `«redacted:<Header>»`, and the credentials they carry are redacted wherever they reappear (an echoing server, the URL). Every value that saving would turn into a placeholder (see [Secrets become placeholders](#saving-captures-as-request-files): secret-named headers and fields, auth scheme values such as `Bearer …`, known token formats under any name) is redacted the same way, in the HAR, the `--json` lines and the TUI alike. In the URL, the placeholder is percent-encoded so it stays a valid URI. `--show-secrets` writes the values as they are; keep such a file out of Git.
- **Comments for what a viewer would misread.** A tunnel that was not decrypted is a `CONNECT https://host:port/` entry with status 0 and the comment `tunnel: not decrypted; <up> B up, <down> B down`. A rejected handshake is a `CONNECT` entry with `TLS REJECTED: <hint>`. A truncated body says `truncated: response body stored <n> of <total> bytes` (or `request`), a decrypted exchange says `decrypted by the Rustman local CA`, and a failed exchange carries its error.

## The TUI recorder pane

`Alt+P` (or **Traffic recorder** in the palette) shows the recorder pane in place of the request and response panes; `Esc` or `Alt+P` hides it, and the proxy keeps running while it is hidden. The pane shows the proxy's state and address, the intercepted host globs, the number of captures and how many are marked; the capture list newest first (time in UTC, method, host, path, status, duration and size; `TUNNEL host:port` with byte counts; `TLS REJECTED host`), and the selected capture: the request line, the HTTP version, status and duration, then the request and response headers and bodies. Everything is redacted as in the [HAR export](#har-export): a captured `Cookie: sid=abc` shows as `Cookie: «redacted:Cookie»`.

| Key | What it does |
|---|---|
| `s` | Start or stop the proxy. It binds `[record] host` and `port` from `rustman.toml` (default `127.0.0.1:8888`) and decrypts `[record] intercept` hosts with the workspace's local CA (it must exist: run `rustman record ca init`). An address reachable from the network asks for confirmation first (Cancel is preselected); while such a proxy runs, the pane shows `REACHABLE FROM THE NETWORK` and the status bar a red `RECORDER ON NETWORK` badge. |
| `Up`/`Down`, `j`/`k`, `g`/`G` | Select a capture. The top row follows new captures; another selected capture stays selected as captures arrive. |
| `PageUp`/`PageDown` | Scroll the selected capture. |
| `Space` | Mark or unmark the selected capture (`*` in the list). |
| `a` | Mark every shown capture, or unmark them all when they all are. |
| `/` | Filter the list: type text the rows must contain (case-insensitive); `Enter` keeps the filter, `Esc` clears it. |
| `w` | Save the marked captures (or the selected one) as request files, exactly like [`--save`](#saving-captures-as-request-files) with the default folder and the `recorded` environment. Secrets become placeholders; their values are never stored from the TUI (set them with `rustman secret set`). |
| `e` | Export the marked captures (or the selected one) as HAR, redacted: asks for the file name (`recording.har` first), relative to the workspace root. |

The status bar shows `record <addr>` while the proxy runs, and a `CA TRUSTED` badge in the warning colour while Rustman has this workspace's CA installed in a trust store (read when the pane is opened and when the proxy starts), as a reminder to run `rustman record ca uninstall` when you are done.
