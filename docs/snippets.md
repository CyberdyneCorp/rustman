# Code snippets

`rustman export snippet --lang <lang> <request>` prints a request as code for another language or tool, with its variables filled in for the active environment. It uses the same resolved request as [`rustman export curl`](http.md#export), and the same redaction.

## Languages

`rustman export snippet --list` prints the supported languages:

| Id | Prints | Runs with |
|----|--------|-----------|
| `curl` | the `rustman export curl` command | curl |
| `httpie` | an `http` command | HTTPie 3.2 or later |
| `wget` | a `wget` command | GNU Wget |
| `python-requests` | a script using `requests` | Python 3 with `requests` |
| `python-httpx` | a script using `httpx` | Python 3 with `httpx` |
| `js-fetch` | an ES module using `fetch` | Node.js 20 or later (or a browser, without files) |
| `node-axios` | an ES module using `axios` | Node.js with `axios` |
| `go` | a `main.go` using `net/http` | Go |
| `rust-reqwest` | a `main.rs` using blocking `reqwest` | Rust, with the `reqwest` line from the snippet's first comment |
| `powershell` | `Invoke-WebRequest` with splatted parameters | PowerShell 7 |
| `csharp` | top-level statements using `HttpClient` | .NET 10 (`dotnet run snippet.cs`) |

`fetch`, `python` and `rust` are accepted as aliases of `js-fetch`, `python-requests` and `rust-reqwest`. Any other id exits with code 2 and lists the supported ones:

```text
error: snippet: unknown language 'cobol' (supported: curl, httpie, wget, python-requests, python-httpx, js-fetch, node-axios, go, rust-reqwest, powershell, csharp)
```

## Example

```yaml
# users/create.rman.yaml
version: 1
kind: http
name: create-user
method: POST
url: '{{base_url}}/users'
auth:
  type: bearer
  token: '{{api_token}}'
body:
  type: json
  content:
    name: Ada
    admin: false
```

With `base_url: https://api.example.com` and the secret `api_token` set for `prod`:

```console
$ rustman export snippet --lang python-requests users/create --env prod
import requests

response = requests.post(
    "https://api.example.com/users",
    json={"name": "Ada", "admin": False},
    headers={
        "Authorization": "Bearer «redacted:api_token»",
        "Content-Type": "application/json",
    },
)
print(response.status_code)
print(response.text)
```

Every snippet sends the method, URL (query included), headers and body Rustman would send, plus the `Content-Type` the body implies. A multipart body names its files relative to the workspace root, so run the snippet from there. Redirects and `verify_tls: false` are carried over where the language has a switch for them; `js-fetch` and `node-axios` cannot turn TLS verification off and print a warning instead. Two multipart limits also print a warning: `wget` cannot send multipart bodies at all (the snippet says so in a comment), and `httpie` sends a text part without its own `Content-Type`.

Rustman sends a non-ASCII header value (`X-Label: café`) as UTF-8 bytes. Python's and Node's clients would write a text value as Latin-1 (or refuse it), and reqwest refuses one, so their snippets pass the bytes: `"café".encode()` in Python, `String.fromCharCode(...new TextEncoder().encode("café"))` in JavaScript, `"café".as_bytes()` in Rust. The .NET client behind `powershell` and `csharp` may refuse such a value, so those print a warning: `warning: snippet: csharp may refuse the non-ASCII value of header X-Label`. A JSON body can be re-encoded by the client (Python's `json=` writes `é` as `\u00e9` and adds spaces); the document sent is the same.

### Values stay text

Whatever a header, query, form or multipart value holds, the snippet sends it as text and runs nothing:

- Shell snippets (`curl`, `httpie`, `wget`) single-quote every word that is not plain letters, digits and `-_=/,.+`, so `$(...)`, backticks, `!` and line breaks are not expanded; a word starting with `=` is quoted too (zsh expands `=name`). No user value can start an option: `httpie` always writes `--` before the method, so a header or part name starting with `-` (`-oFILE`, `--session-read-only=…`) is a request item; `wget` always writes `--` before the URL; `curl` gives every value as the argument of its option (`-H`, `--form-string`, `--data-raw`) and a URL starting with `-` after `--url`. A shell argument cannot carry a NUL byte, so a request holding one (in its body, for example) gets a comment instead of a command, with a warning: send it with rustman or put the body in a file.
- `httpie` reads `:`, `;`, `=` and `@` in a request item as separators, and a header value starting with `=@` or `:=@` (or a part value starting with `@`) as "read this file". The snippet backslash-escapes every such character in a name and the first character of a value, writes an empty header as `Name;` (`Name:` would leave it out), and passes the body as `--raw=<text>`. httpie cannot write a backslash right before one of those characters: such a header or part is left out with a warning. A method that is not made of letters is read by httpie as the URL, so the snippet is a comment and a warning instead.
- `curl` writes text parts with `--form-string`, which never reads a file, and quotes `-F` values. An empty header becomes `-H 'Name;'` (curl drops `Name:`). A header name that is not an HTTP token (`-H @file` reads headers from a file), a part name holding `=`, and a part content type other than a plain `type/subtype; param=value` (curl reads `;headers=@file` after it) are left out with a warning.
- Code snippets write every value as a string literal of the language (C# also escapes U+0085, U+2028 and U+2029, which end a line there).

### Known fidelity limits

Snippets send the same request as rustman except in these cases:

- `node-axios` adds `Content-Type: application/x-www-form-urlencoded` to a `POST` without a body.
- `js-fetch` and `node-axios` send a text part that has a content type as a file part (a `Blob`), with a file name.
- `go` escapes `"` in a part name differently (`\"` instead of `%22`).
- A backslash in a part name is doubled in `python-requests`, `python-httpx` and `go`.
- JavaScript reads a JSON body into numbers, so huge integers lose precision and `-0` becomes `0`.
- File paths holding `\`, and URLs with invalid percent-escapes, may be read differently by each tool.

## Secrets

Secrets are replaced by `«redacted:<name>»` unless `--show-secrets` is passed, as are the credentials in `Authorization`, `Cookie` and the other credential headers. In code snippets a credential header keeps its scheme, as in `"Bearer «redacted:api_token»"`; the `curl` snippet is exactly the `rustman export curl` command. Warnings go to stderr and never contain secret values.

## Signature auth

Digest, AWS SigV4 and JWT auth are computed per request (from a server challenge or the current time), so they cannot be written as a fixed header. A snippet uses the language's own helper where there is one:

| Auth | Helper |
|------|--------|
| `digest` | `--digest -u` in curl, `--auth-type digest --auth=` in httpie, `--user`/`--password` in wget, `HTTPDigestAuth` in python-requests, `httpx.DigestAuth` in python-httpx |
| `aws_sigv4` | `--aws-sigv4 'aws:amz:<region>:<service>' -u` in curl |
| `jwt` | none |

Otherwise the snippet starts with a comment naming the auth type, and stderr has a warning:

```console
$ rustman export snippet --lang go aws/list-buckets --env prod
warning: snippet: aws_sigv4 auth cannot be precomputed for go
// aws_sigv4 auth: sign with the AWS SDK signer
package main
...
```

## Placeholder mode

`--placeholders` keeps variables unresolved, to show how a request is built rather than one instance of it: `{{name}}` in curl, httpie and wget, and constants declared at the top of code snippets. Secrets are never read in this mode and always print as `«redacted:<name>»` (when the environment declares them), so `--show-secrets` cannot be combined with it.

```yaml
# users/get.rman.yaml
version: 1
kind: http
name: get-user
url: '{{base_url}}/users/{{id}}'
```

```console
$ rustman export snippet --lang python --placeholders users/get --env prod
import requests

BASE_URL = "https://api.example.com"
ID = "42"

response = requests.get(
    BASE_URL + "/users/" + ID,
)
print(response.status_code)
print(response.text)
$ rustman export snippet --lang httpie --placeholders users/get --env prod
http --ignore-stdin --follow -- GET '{{base_url}}/users/{{id}}'
```

A constant takes the variable's value in the selected environment, or `{{name}}` when it has none.

## Request kinds

`kind: http` and `kind: graphql` requests have snippets. A GraphQL request exports as the HTTP POST it sends: `{"query", "operationName", "variables"}` as JSON, with `Accept: application/graphql-response+json, application/json`. The document is never interpolated; its `variables` are. Any other kind exits with code 2:

```text
error: snippet: kind 'mcp' has no snippet export
```

## Library use

The snippet engine lives in `rustman-http` (`rustman_http::snippet`), so the CLI, the TUI and generated docs share it: `snippet(&resolved, lang, &options)` renders one language from a resolved request, and `resolve_placeholders` resolves a request for placeholder mode. Each language is one `minijinja` template in `crates/rustman-http/src/snippet/templates/` plus its quoting rules.
