# HTTP requests

How Rustman sends a `kind: http` request: transport and timeouts, redirects, TLS, proxies, cookies, auth helpers, the timing breakdown, response bodies, and curl import and export. The request file format itself is in [request-files.md](request-files.md); variables and secrets are in [environments.md](environments.md).

The examples on this page are tested. The YAML files are written into one workspace that must load, every `$ rustman ...` line runs against it and must print the lines shown (`crates/rustman-cli/tests/docs_http.rs`), and the messages listed under [Errors and warnings](#errors-and-warnings) are produced by the engine (`crates/rustman-http/tests/docs_http.rs`).

## Transport

Rustman uses its own HTTP stack (hyper with a timed connector and rustls); it does not use the system's curl or TLS libraries.

- `https://` URLs negotiate HTTP/2 through ALPN and fall back to HTTP/1.1.
- `http://` URLs use HTTP/1.1, unless `settings.http2_prior_knowledge: true` (HTTP/2 over cleartext, "h2c").
- Connections are reused between requests that share the same TLS, proxy and timeout settings.
- When a host name resolves to several addresses (for example `localhost` as `::1` and `127.0.0.1`), Rustman tries them in resolver order and starts the next one if an attempt fails or has not connected within 250 ms; the first to connect is used (Happy Eyeballs, RFC 8305).

The exchange records the negotiated `http_version` (`HTTP/1.1` or `HTTP/2`).

### Timeouts

| Setting | Default | Covers |
|---------|---------|--------|
| `timeout_ms` | 30000 | The whole exchange: connect, send, response and body. Set in `rustman.toml [defaults]` or the request's `settings.timeout_ms`; `--timeout` on `rustman send` and `rustman run` replaces both. |
| `connect_timeout_ms` | 10000 | DNS, TCP connect, proxy tunnel and TLS handshake. Set in `settings.connect_timeout_ms`. |

A timeout fails the request with `timeout after <n> ms (<phase>)`, where the phase is `connect` or `response`.

### Redirects

Redirects are followed by default, up to `max_redirects` (10) hops, and every hop is recorded in the exchange. A `301`, `302` or `303` turns a non-`GET`/`HEAD` request into a `GET` without a body; `307` and `308` keep the method and body. Credentials are dropped when a hop goes to another origin (scheme, host or port): `Authorization`, `Proxy-Authorization`, an explicit `Cookie` header (the cookie jar still sends the cookies that match the new host) and the `api_key` header or query parameter. Any other header whose value carries a secret (such as `X-Api-Key: '{{token}}'`), and a `307`/`308` body that carries one (such as a form with a `client_secret`), are dropped too, so a secret never follows a redirect to another server; the hop is still sent, without them, and the exchange gets the warning `cross-origin redirect to <origin>: dropped header x-api-key, the body, which carry a secret`. A secret is found the way output is redacted: a secret variable's value, or a literal credential in `Authorization`, `Cookie` and the like, in any of its encoded forms. An `aws_sigv4` request is signed again on each same-origin hop and not at all once a hop leaves the origin (see [SigV4](auth.md#aws-sigv4)). Same-origin hops keep everything. With `follow_redirects: false` the 3xx response is returned as is. Too many hops fail with `too many redirects (<max>)`.

```yaml
# http/no-redirects.rman.yaml
version: 1
kind: http
name: Login redirect
url: https://app.example.com/login
settings:
  follow_redirects: false
  max_redirects: 3
```

### TLS

Certificates and host names are verified against the operating system's trust store, plus any `ca_bundle` (PEM) from `rustman.toml [http]` and the request's `settings.ca_bundle`. With a `ca_bundle`, the operating system's root certificates are loaded as a list next to the bundle, so OS-only checks such as revocation do not apply to that request. A failure starts with `tls: certificate verification failed`.

`verify_tls: false` (in `[defaults]` or the request's settings) or `--insecure` on `rustman send` and `rustman run` turns verification off. Every exchange sent that way is flagged `insecure: true`, plain `http://` ones included.

For mutual TLS, give a PEM certificate and key, or a PKCS#12 bundle whose passphrase comes from a secret variable:

```yaml
# http/mtls.rman.yaml
version: 1
kind: http
name: Internal API with a client certificate
url: https://internal.example.com/health
settings:
  ca_bundle: ../certs/dev-ca.pem
  client_cert: ../certs/client.pem
  client_key: ../certs/client.key
```

A client certificate in `rustman.toml [http]` (`client_cert` and `client_key`, or `client_pkcs12` and `client_pkcs12_password`, with paths relative to the workspace root) applies to every request that sets none of its own; see [request-files.md](request-files.md#rustmantoml). The certificate is presented to the origin server only, never to an `https://` proxy on the way.

```yaml
# http/mtls-p12.rman.yaml
version: 1
kind: http
name: Internal API with a PKCS#12 bundle
url: https://internal.example.com/health
settings:
  client_pkcs12: ../certs/client.p12
  client_pkcs12_password: '{{p12_password}}'
```

### Proxies

The proxy for a request is, in order: `settings.proxy`, `rustman.toml [http] proxy`, then the `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` environment variables (either case). `NO_PROXY` lists hosts that bypass a proxy taken from the environment. Supported schemes are `http://`, `https://` and `socks5://`; `https://` URLs go through an HTTP proxy with `CONNECT`. `settings.proxy: none` bypasses every proxy.

```yaml
# http/via-socks.rman.yaml
version: 1
kind: http
name: Through the SOCKS tunnel
url: https://api.internal.local/status
settings:
  proxy: socks5://127.0.0.1:1080
```

### Cookies

Each environment has its own cookie jar, `.rustman/cookies/<env>.json`, kept across runs and following the RFC 6265 domain, path, expiry and `Secure` rules. A `Set-Cookie` received under `staging` is never sent with `--env prod`. `settings.cookies: false` leaves the jar out of one request, and an explicit `Cookie` header replaces the jar's cookies. Jar files are readable by the current user only, and are written only when `.rustman/` is a real folder (not a symbolic link) that Git ignores.

```console
$ rustman cookies clear --env prod
no cookies stored for environment 'prod'
```

## Headers

Unless the request sets them, every request carries `User-Agent: rustman/<version>` and `Accept: */*`. The body type sets a default `Content-Type`, which an explicit header replaces (exactly one is sent):

| Body type | Default `Content-Type` |
|-----------|------------------------|
| `json` | `application/json` |
| `text` | `text/plain; charset=utf-8` |
| `form` | `application/x-www-form-urlencoded` |
| `multipart` | `multipart/form-data; boundary=…` (a random boundary) |
| `file` | `application/octet-stream` |

`Content-Length` and `Host` are always computed from the body and the URL. A request that sets a different value gets the computed one, and the exchange records a warning such as `ignored stale Content-Length: 3 (sent the computed 10)`.

## Auth

Auth is applied after variables are filled in, and never replaces an explicit `Authorization` header (or, for an API key, an explicit header of the same name).

| Type | Sends |
|------|-------|
| `{type: basic, username, password}` | `Authorization: Basic <base64 of username:password>` |
| `{type: bearer, token}` | `Authorization: Bearer <token>` |
| `{type: api_key, name, value, in: header}` | the header `<name>: <value>` (`in` defaults to `header`) |
| `{type: api_key, name, value, in: query}` | the query parameter `<name>=<value>` |
| `{type: oauth2, ...}` | `Authorization: Bearer <access token>` |
| `{type: digest, ...}`, `{type: aws_sigv4, ...}`, `{type: jwt, ...}` | credentials computed per request; see [Signature auth](auth.md) |

When a Basic username or password holds a secret, the encoded credential is redacted too, so a server that echoes the `Authorization` header back cannot leak it.

```yaml
# http/me.rman.yaml
version: 1
kind: http
name: Me
url: '{{base_url}}/me'
auth:
  type: bearer
  token: '{{api_token}}'
```

```yaml
# http/search.rman.yaml
version: 1
kind: http
name: Search
url: '{{base_url}}/search'
query:
  - name: q
    value: rust
auth:
  type: api_key
  name: key
  value: '{{api_key}}'
  in: query
```

### OAuth2

`grant: client_credentials` gets a token from `token_url` with the client id and secret. `grant: authorization_code` also needs `auth_url`: Rustman opens the browser (and prints the URL) with a PKCE `code_challenge_method=S256` challenge, and receives the code on a loopback listener at `http://127.0.0.1:<ephemeral port>/callback`. The listener serves at most 16 connections at once (each gets 5 s to send its request), and when accepting a connection fails (for example when the process is out of file descriptors) it retries after a delay that doubles from 10 ms up to 1 s, rather than spinning.

Tokens are cached per environment in the secret store, never in workspace files. A cached token is reused until shortly before it expires, refreshed with its refresh token when there is one, and acquired again when a request gets a `401`. Calls to `token_url` never follow redirects, so the client secret and codes in the form cannot be re-posted elsewhere; point `token_url` at the final URL.

```yaml
# http/oauth-login.rman.yaml
version: 1
kind: http
name: Profile
url: '{{base_url}}/profile'
auth:
  type: oauth2
  grant: authorization_code
  auth_url: https://auth.example.com/authorize
  token_url: https://auth.example.com/token
  client_id: rustman-cli
  scopes:
    - profile
```

The authorization-code grant needs a person at a terminal. When `rustman send` or `rustman run` has no terminal on stdin (as in CI) and no cached token, the request fails with `oauth2: interactive login required (run 'rustman auth login <request>')`. Run `rustman auth login http/oauth-login --env prod` once on a workstation; later runs reuse and refresh the cached token. When the token goes to `.rustman/secrets.yaml` rather than the OS keychain (no keychain, or `RUSTMAN_SECRET_STORE=file`), `rustman auth login` says so once: `warning: OAuth2 tokens are stored in .rustman/secrets.yaml (git-ignored, readable by you only), not in the OS keychain`. See [secret storage](environments.md#secret-storage).

## Timing breakdown

Each exchange records, in milliseconds with microsecond precision:

| Phase | From → to |
|-------|-----------|
| `dns` | name lookup |
| `connect` | TCP connect (and proxy tunnel) |
| `tls` | TLS handshake |
| `ttfb` | connection ready (request start on a reused connection) → first response byte |
| `download` | first response byte → last body byte |
| `total` | the whole exchange; at least the sum of the phases |
| `auth` | a Digest challenge round trip answered before the final one (absent otherwise); `total` includes it |

Phases that did not happen are `null`, never `0`: `tls` on plain HTTP, `dns` for an IP address, and `dns`, `connect` and `tls` on a reused connection.

## Response bodies

- Bodies are decompressed (`gzip`, `br`, `deflate`, `zstd`) unless `settings.decompress: false`. An unknown `Content-Encoding` leaves the body as received, with a warning. Decoding goes through a fixed 64 KiB buffer, so memory stays bounded whatever the compression ratio, and a body that decodes to more than 1 GiB fails with `network: the decompressed response body is larger than <n> bytes` (use `decompress: false` to keep such a body encoded).
- A body that is not decoded (no `Content-Encoding`, or `decompress: false`) is limited by `rustman.toml [http] max_download_bytes` (1 GiB): past it the request fails with `network: the response body is larger than <n> bytes`, so an endless stream cannot fill the disk before the timeout.
- Bodies up to `rustman.toml [http] max_body_bytes` (10 MiB) stay in memory. A larger body is streamed to `.rustman/bodies/<uuid>.bin` (a random name per exchange; outside a workspace, a folder of the current user in the temp folder: `rustman-bodies-<uid>` on Unix, `rustman-bodies` in the per-user temp folder on Windows); the exchange keeps its first 64 KiB as a preview and its full `size_bytes`. The file is removed again when the request fails or times out before the body ends, and redirect hops keep none. Once the request is done, the file is kept only while a history entry refers to it: with `--no-history`, history disabled or outside a workspace, it is deleted when the command has used it (after `rustman send` printed the body). Pruning history deletes it with its entry, along with any spill file no entry refers to that is more than an hour old, and `rustman history clear` deletes every spill file. Spill files are readable by the current user only, and, like cookie jars, are written only when `.rustman/` is a real, git-ignored folder. A spill folder that is a symbolic link, or (on Unix) belongs to another user, is refused; one of ours that others can access is set back to mode `0700`. A spill file is always created new, never opened when something already exists at its path. A 50 MiB download keeps well under 16 MiB of memory for its body.
- A body that is not valid UTF-8 (an image, a PDF) is marked `binary: true`, and its bytes are kept exactly as received, never decoded lossily.

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[http]
max_body_bytes = 1048576   # spill bodies over 1 MiB
max_download_bytes = 104857600   # fail undecoded bodies over 100 MiB
```

## Curl import and export

### Import

`rustman import curl` reads a curl command, from one argument, from its words after `--`, or from stdin, and prints the equivalent request file. `--out <file>` writes it instead (an existing file is never replaced).

```console
$ rustman import curl "curl -X POST https://api.example.com/users -H 'Content-Type: application/json' -d '{\"name\":\"ana\",\"admin\":false}'"
version: 1
kind: http
name: POST /users
method: POST
url: https://api.example.com/users
body:
  type: json
  content:
    name: ana
    admin: false
```

| curl option | Becomes |
|-------------|---------|
| `-X`, `--request` | `method` |
| URL, `--url` | `url` (`http://` is added when the scheme is missing, unless the URL starts with a variable such as `{{base_url}}`) |
| `-H`, `--header` | `headers` |
| `-d`, `--data`, `--data-ascii`, `--data-binary`, `--data-raw`, `--data-urlencode`, `--json` | `body`: `json` when the content type is JSON, `form` for `a=1&b=2` data, a `file` body for `@file`, else `text` |
| `-F`, `--form`, `--form-string` | a `multipart` body (`@file` parts and `;type=` included) |
| `-u`, `--user` | `auth: {type: basic}` |
| `-b`, `--cookie` | a `Cookie` header (cookie files are not read) |
| `-k`, `--insecure` | `settings.verify_tls: false` |
| `-L`, `--location` | `settings.follow_redirects: true` |
| `--compressed` | an `Accept-Encoding` header (responses are always decompressed) |
| `-G`, `--get` | the `-d` data becomes `query` parameters of a `GET` (an `@file` is not read) |

Data without `-X` makes the method `POST`, as in curl. A `Content-Type` header that the body type implies is left out of the file. Every other option is reported, not silently dropped:

```console
$ rustman import curl "curl --limit-rate 1k https://api.example.com/health"
warning: ignored unsupported curl option --limit-rate
version: 1
kind: http
name: GET /health
method: GET
url: https://api.example.com/health
```

### Export

`rustman export curl <request>` prints the request, with its variables filled in for the active environment, as a single POSIX-shell-quoted curl command. Secrets are replaced by `«redacted:<name>»` unless `--show-secrets` is passed, as are the values of `Authorization`, `Cookie` and the other credential headers. A request holding a NUL byte (in its body, for example) is printed as a comment with a warning instead, since a shell argument cannot carry one.

```yaml
# environments/prod.yaml
variables:
  base_url: https://api.example.com
secrets:
  - api_token
  - api_key
  - p12_password
```

With the secret `api_token` set for `prod`:

```console
$ rustman export curl http/me --env prod
curl 'https://api.example.com/me' -H 'Authorization: «redacted:api_token»' -L
$ rustman export curl http/no-redirects
curl 'https://app.example.com/login'
```

Exporting and importing back gives the same method, URL, headers and body. An OAuth2 token is not part of the export (a warning says so), since it only exists once the request runs. For other languages and tools, see [Code snippets](snippets.md).

## Errors and warnings

The messages on this page, as Rustman prints them (`<...>` marks the variable part):

```text
timeout after 500 ms (response)
too many redirects (10)
tls: certificate verification failed for <host>: <reason>
network: the decompressed response body is larger than <n> bytes
network: the response body is larger than <n> bytes
oauth2: interactive login required (run 'rustman auth login <request>')
ignored stale Content-Length: 3 (sent the computed 10)
ignored Host: <value> (sent the URL's host <host>)
cross-origin redirect to <origin>: dropped <parts>, which carry a secret
ignored unsupported curl option <option>
```

Request failures (network, TLS, timeout, proxy, auth) exit with code 3; a curl command that cannot be imported exits with code 2.
