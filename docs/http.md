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
| `timeout_ms` | 30000 | The whole exchange: connect, send, response and body. Set in `rustman.toml [defaults]` or the request's `settings.timeout_ms` (and with `--timeout` on `rustman send`, which arrives with the CLI commands). |
| `connect_timeout_ms` | 10000 | DNS, TCP connect, proxy tunnel and TLS handshake. Set in `settings.connect_timeout_ms`. |

A timeout fails the request with `timeout after <n> ms (<phase>)`, where the phase is `connect` or `response`.

### Redirects

Redirects are followed by default, up to `max_redirects` (10) hops, and every hop is recorded in the exchange. A `301`, `302` or `303` turns a non-`GET`/`HEAD` request into a `GET` without a body; `307` and `308` keep the method and body. Credentials are dropped when a hop goes to another origin (scheme, host or port): `Authorization`, `Proxy-Authorization`, an explicit `Cookie` header (the cookie jar still sends the cookies that match the new host) and the `api_key` header or query parameter. With `follow_redirects: false` the 3xx response is returned as is. Too many hops fail with `too many redirects (<max>)`.

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

`verify_tls: false` (in `[defaults]` or the request's settings, and `--insecure` on `rustman send` once it arrives) turns verification off. Every exchange sent that way is flagged `insecure: true`, plain `http://` ones included.

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

`grant: client_credentials` gets a token from `token_url` with the client id and secret. `grant: authorization_code` also needs `auth_url`: Rustman opens the browser (and prints the URL) with a PKCE `code_challenge_method=S256` challenge, and receives the code on a loopback listener at `http://127.0.0.1:<ephemeral port>/callback`.

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

The authorization-code grant needs a person at a terminal. Without one (stdin is not a TTY, as in CI) and without a cached token, the request fails with `oauth2: interactive login required (run 'rustman auth login <request>')`. Run `rustman auth login http/oauth-login --env prod` once on a workstation; later runs reuse and refresh the cached token.

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

Phases that did not happen are `null`, never `0`: `tls` on plain HTTP, `dns` for an IP address, and `dns`, `connect` and `tls` on a reused connection.

## Response bodies

- Bodies are decompressed (`gzip`, `br`, `deflate`, `zstd`) unless `settings.decompress: false`. An unknown `Content-Encoding` leaves the body as received, with a warning. Decoding goes through a fixed 64 KiB buffer, so memory stays bounded whatever the compression ratio, and a body that decodes to more than 1 GiB fails with `network: the decompressed response body is larger than <n> bytes` (use `decompress: false` to keep such a body encoded).
- Bodies up to `rustman.toml [http] max_body_bytes` (10 MiB) stay in memory. A larger body is streamed to `.rustman/bodies/<exchange-id>.bin`; the exchange keeps its first 64 KiB as a preview and its full `size_bytes`. The file is removed again when the request fails or times out before the body ends, and redirect hops keep none. Spill files are readable by the current user only, and, like cookie jars, are written only when `.rustman/` is a real, git-ignored folder. A 50 MiB download keeps well under 16 MiB of memory for its body.
- A body that is not valid UTF-8 (an image, a PDF) is marked `binary: true`, and its bytes are kept exactly as received, never decoded lossily.

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[http]
max_body_bytes = 1048576   # spill bodies over 1 MiB
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
| URL, `--url` | `url` (`http://` is added when the scheme is missing) |
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

`rustman export curl <request>` prints the request, with its variables filled in for the active environment, as a single POSIX-shell-quoted curl command. Secrets are replaced by `«redacted:<name>»` unless `--show-secrets` is passed, as are the values of `Authorization`, `Cookie` and the other credential headers.

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

Exporting and importing back gives the same method, URL, headers and body. An OAuth2 token is not part of the export (a warning says so), since it only exists once the request runs.

## Errors and warnings

The messages on this page, as Rustman prints them (`<...>` marks the variable part):

```text
timeout after 500 ms (response)
too many redirects (10)
tls: certificate verification failed for <host>: <reason>
network: the decompressed response body is larger than <n> bytes
oauth2: interactive login required (run 'rustman auth login <request>')
ignored stale Content-Length: 3 (sent the computed 10)
ignored Host: <value> (sent the URL's host <host>)
ignored unsupported curl option <option>
```

Request failures (network, TLS, timeout, proxy, auth) exit with code 3; a curl command that cannot be imported exits with code 2.
