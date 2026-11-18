# Signature auth: Digest, AWS SigV4 and JWT

Besides `basic`, `bearer`, `api_key` and `oauth2` (see [HTTP requests](http.md#auth)), a request or `_collection.yaml` can use three auth types whose credentials are computed for every request: `digest`, `aws_sigv4` and `jwt`. Their fields are interpolated like any other auth field, so keys and passwords normally come from [secret variables](environments.md). None of them replaces an explicit `Authorization` header.

| Type | Sends |
|------|-------|
| `{type: digest, username, password}` | `Authorization: Digest ...`, answering the server's `401` challenge |
| `{type: aws_sigv4, region, service, ...}` | `Authorization: AWS4-HMAC-SHA256 ...`, `X-Amz-Date`, `X-Amz-Content-Sha256` and, with a session token, `X-Amz-Security-Token` |
| `{type: jwt, algorithm, ...}` | `Authorization: Bearer <jwt>` (or a named header, or a query parameter) with a token signed for this request |

## Digest

```yaml
# auth/digest.rman.yaml
version: 1
kind: http
name: Digest
url: '{{base_url}}/protected'
auth:
  type: digest
  username: ana
  password: '{{password}}'
```

Digest follows RFC 7616:

1. The first request goes out without credentials.
2. On a `401` with `WWW-Authenticate: Digest`, Rustman computes the answer and sends the request once more, with `nc=00000001` and a fresh `cnonce`. A second `401` (a wrong password, for example) is returned as the response; there is no further retry.
3. The nonce is cached per environment, host and realm for the rest of the process (a `rustman run`), so later requests to the same host send credentials up front with the next count (`nc=00000002`, ...). A `401` with `stale=true` (or any `401` to such a request) gets one retry with the new nonce, counting from `00000001` again.

Supported: `MD5`, `MD5-sess`, `SHA-256`, `SHA-256-sess`, `qop=auth`, and challenges without `qop` (RFC 2069). When the server offers several `Digest` challenges (one per algorithm, in one or several `WWW-Authenticate` headers), Rustman answers the first supported one, preferring `SHA-256` (and `SHA-256-sess`) over `MD5`; if none is supported, the first one's error is reported. A challenge that only offers unsupported `qop` values fails naming them, for example `digest: unsupported qop auth-int`; one with `userhash=true` fails with `digest: unsupported userhash`.

Because the request may be sent twice, a `file` or `multipart` body is buffered in memory, up to 10 MiB. A larger body fails before anything is sent, with `digest: body larger than 10 MiB cannot be replayed` (exit code 3).

When a challenge was answered, the exchange's [timing breakdown](http.md#timing-breakdown) has `auth`: the duration of the challenge round trip. `total` covers both round trips. In the JSON report these are `auth_ms` and `total_ms`.

## AWS SigV4

```yaml
# auth/s3-get.rman.yaml
version: 1
kind: http
name: S3 object
url: https://bucket.s3.amazonaws.com/a.txt
auth:
  type: aws_sigv4
  region: us-east-1
  service: s3
  access_key: '{{aws_key}}'
  secret_key: '{{aws_secret}}'
```

Requests are signed with the signer of the AWS SDK for Rust (the `aws-sigv4` crate). Signing is the last step before a request is sent, after variables, default headers, collection headers and cookies, so all of them are part of `SignedHeaders` (except `User-Agent`, `Proxy-Authorization` and the other headers proxies may change). Every redirect hop on the request's origin (same scheme, host and port) is signed again for its own URL. A redirect to another origin is followed unsigned: that hop and every later one, even one that comes back to the original origin, carry no `Authorization`, `X-Amz-Date`, `X-Amz-Content-Sha256` or `X-Amz-Security-Token`, so another server never receives a signature, the access key or the session token. For `service: s3` the path is signed as sent; other services use SigV4's double encoding and path normalization. The query string is canonicalized by the same crate; a literal `+` or `*` in a query value may be canonicalized differently by other SigV4 implementations (botocore, for example), so percent-encode them (`%2B`, `%2A`) when the server must verify the signature.

Credentials come from one of three sources:

| Fields | Credentials |
|--------|-------------|
| `access_key`, `secret_key`, optional `session_token` | the given values, usually secret variables |
| `from: env` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and optional `AWS_SESSION_TOKEN` |
| `from: profile`, `profile: <name>` | the static keys of that profile in `~/.aws/credentials` (or `~/.aws/config`; `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` override the paths) |

A profile that uses SSO, an assumed role or `credential_process` fails, naming the key: `aws_sigv4: profile 'dev' uses sso_start_url, which is not supported; export temporary credentials or use a secret provider`. The secret key and session token, whatever their source, are redacted everywhere as `«redacted:aws_secret_key»` and `«redacted:aws_session_token»`.

```yaml
# auth/api-gateway.rman.yaml
version: 1
kind: http
name: API Gateway
url: https://abc123.execute-api.eu-west-1.amazonaws.com/prod/items
auth:
  type: aws_sigv4
  region: eu-west-1
  service: execute-api
  from: env
```

`X-Amz-Content-Sha256` is the SHA-256 of the exact body bytes sent. `unsigned_payload: true` sends `UNSIGNED-PAYLOAD` instead and skips hashing the body, which S3 accepts for large uploads. The body itself is still read into memory before it is sent, as every request body is. When AWS rejects a request because of the clock (`RequestTimeTooSkewed` or `Signature expired`), the exchange gets the warning `check the system clock; request was signed at <X-Amz-Date>`.

## JWT

```yaml
# auth/jwt.rman.yaml
version: 1
kind: http
name: JWT
url: '{{base_url}}/me'
auth:
  type: jwt
  algorithm: HS256
  secret: '{{jwt_secret}}'
  claims:
    sub: '{{user_id}}'
    scope: read
```

A new token is signed for every request, so two requests a few seconds apart carry different tokens. The claims are interpolated (`user_id=42` gives `"sub": "42"`), and `iat` (now) and `exp` (`iat + ttl`) are added unless `claims` sets them.

| Field | Meaning |
|-------|---------|
| `algorithm` | `HS256`, `HS384`, `HS512`, `RS256` or `ES256` |
| `secret` | the HMAC secret (HS algorithms only), usually a secret variable; a secret that resolves to empty text fails the request with `jwt: HS256 secret is empty` |
| `private_key` | a PEM private key (RS256, ES256), usually a secret variable |
| `private_key_file` | a PEM private key file, relative to the workspace root; it may not leave the workspace |
| `claims` | the token's claims |
| `header` | extra JOSE header fields: `kid`, `typ`, `cty`, `jku`, `jwk`, `x5u`, `x5c`, `x5t`, `x5t#S256` |
| `ttl` | the lifetime: `30s`, `5m` (the default), `1h` |
| `in`, `name` | `in: header` (default) sends `Authorization: Bearer <jwt>`, or the bare token in the header `name`; `in: query` sends it as the query parameter `name` |

RS256 needs an RSA key and ES256 an EC (P-256) key in PKCS#8 form (`-----BEGIN PRIVATE KEY-----`). A key file of the wrong kind fails when the workspace loads, with `jwt: RS256 requires an RSA private key`.

```yaml
# auth/jwt-rs256.rman.yaml
version: 1
kind: http
name: JWT RS256
url: '{{base_url}}/me'
auth:
  type: jwt
  algorithm: RS256
  private_key_file: keys/service.pem
  claims:
    iss: rustman
  header:
    kid: key-1
  ttl: '1h'
```

Generated tokens never reach history, reports or the terminal: they are shown as `«redacted:jwt»`, and history records the header as `Authorization: Bearer «redacted:jwt»`.
