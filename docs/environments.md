# Environments, variables and secrets

An **environment** is a named set of variables, such as `local`, `staging` or `prod`. Request files refer to variables as `{{name}}`, and Rustman fills them in when a request is sent. Some variables are **secrets**: their values never live in workspace files. They're stored in your OS keychain (or a private, git-ignored file) and redacted everywhere they could leak.

The examples on this page are checked by an integration test (`crates/rustman-cli/tests/docs_environments.rs`): the files below make up one workspace, every `$ rustman` command runs against it in order with the output shown, and the error messages shown are the ones Rustman produces. Only the interactive prompt and the CI configuration snippet are not run.

## Environment files

Each file `environments/<name>.yaml` defines one environment. The name is the file stem and must match `[A-Za-z0-9_-]+`; a file such as `environments/my env.yaml` is skipped with `warning: ignoring environment 'my env': invalid name`. A symlinked file is followed when its target is inside the workspace; one that resolves outside it is skipped with `warning: ignoring environment '<name>': resolves outside the workspace`. A dangling symlink (or a folder named `<name>.yaml`) is skipped with `warning: ignoring environment '<name>': not a readable file`.

```yaml
# environments/local.yaml
variables:
  base_url: http://localhost:8080
  page_size: 50
  verbose: true
```

`variables` holds string, number or boolean values. Only `true` and `false` are booleans, and a number with a leading zero stays text, so `0755` and `no` are kept exactly as written. An integer too large for 64 bits can't be used exactly, so it is a load error; quote it to keep it as text.

`secrets` lists variables whose values are stored outside the workspace. A secret must not also have a value under `variables`.

```yaml
# environments/prod.yaml
variables:
  base_url: https://api.example.com
  page_size: 100
secrets:
  - api_token
```

```yaml
# environments/ci.yaml
variables:
  base_url: http://127.0.0.1:9000
  page_size: 10
secrets:
  - api_token
```

Workspace-wide defaults go in `rustman.toml`:

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[defaults]
default_environment = "local"

[variables]
api_version = "v1"
```

A request uses variables anywhere a value is written:

```yaml
# users/me.rman.yaml
version: 1
kind: http
name: Me
method: GET
url: '{{base_url}}/{{api_version}}/me'
query:
  - name: limit
    value: '{{page_size}}'
headers:
  - name: Authorization
    value: Bearer {{api_token}}
```

## The active environment

At most one environment is active. It is chosen by, in order:

1. `--env <name>`
2. the `RUSTMAN_ENV` environment variable
3. `[defaults] default_environment` in `rustman.toml`
4. otherwise none.

`rustman env list` lists the environments and marks the active one with `*`:

```console
$ rustman env list
  ci
* local
  prod
$ RUSTMAN_ENV=prod rustman env list
  ci
  local
* prod
```

Naming an environment that doesn't exist fails with exit code 2:

```console
$ rustman env list --env qa
error: environment 'qa' not found (available: ci, local, prod)
```

## Variables

### Precedence

When a name is defined in several places, the first source in this list wins:

| # | Source |
|---|--------|
| 1 | Runtime overrides: `--var name=value` |
| 2 | Captures from earlier requests in the same run |
| 3 | The request file's `variables` |
| 4 | `_collection.yaml` `variables`, nearest folder first |
| 5 | Persisted captures (`.rustman/state/<env>.yaml`) |
| 6 | The active environment, including its secrets |
| 7 | `rustman.toml` `[variables]` |

Names are case-sensitive: `{{Token}}` and `{{token}}` are different variables.

### Where and how `{{name}}` is filled in

`{{name}}` is filled in in the URL, query parameters, headers, body content, form and multipart text fields, auth fields and file paths.

- **Escaping:** write `\{{` for a literal `{{`. The text `\{{not_a_var}}` is sent as `{{not_a_var}}`.
- **No recursive expansion:** a value is inserted exactly as written. If `a` is `{{b}}`, then `{{a}}` becomes the literal text `{{b}}`. This also applies to values in environment files: `{{...}}` inside a value is never expanded.
- **Unresolved names are errors:** if any placeholder has no value, the request is not sent at all (no connection is opened), and every missing name is listed: `error: unresolved variable(s): base_url, token`. The runner counts this as an error, not as a failed assertion.

### Dynamic variables

These are computed again for every occurrence:

| Placeholder | Value |
|-------------|-------|
| `{{$uuid}}` | A random UUID v4 |
| `{{$timestamp}}` | Unix time in seconds |
| `{{$timestampMs}}` | Unix time in milliseconds |
| `{{$isoTimestamp}}` | RFC 3339 UTC time, e.g. `2026-11-17T09:30:00.123Z` |
| `{{$randomInt}}` | An integer from 0 to 1000, inclusive |
| `{{$randomInt min max}}` | An integer from `min` to `max`, inclusive |
| `{{$randomString n}}` | `n` random letters and digits (16 if `n` is omitted) |
| `{{$env NAME}}` | The OS environment variable `NAME`; unresolved if it isn't set |

## Secrets

### Declaring and setting a secret

List the variable under `secrets` in the environment file (as `api_token` in `prod.yaml` above), then store its value:

```text
$ rustman secret set api_token --env prod
value for api_token (prod):
stored secret 'api_token' for environment 'prod' in the OS keychain
```

On a terminal the value is prompted for without echo. When stdin is not a terminal, the value is read from stdin instead, without its final line break, which is how scripts and CI set secrets (see [the CI pattern](#the-ci-pattern)). Without `--env`, the active environment is used. Only names listed under the environment's `secrets` can be set, and an empty value is refused.

`rustman secret list` shows each declared secret and whether a value is stored. Values are never printed:

```console
$ rustman secret list
ci/api_token  not set
prod/api_token  not set
```

`rustman secret rm api_token --env prod` deletes a stored value.

Until a value is stored, a request that uses the secret fails without being sent:

```text
error: unresolved variable(s): api_token (secret not set; run 'rustman secret set api_token --env prod')
```

If the value is stored but can't be read (the keychain refused access, or `.rustman/secrets.yaml` is malformed), the hint names the store error instead, for example `api_token (could not read secret: OS keychain error: ...)`.

### Where secrets are stored

- **OS keychain** (the default): the macOS Keychain, the Windows Credential Manager, or the Secret Service (GNOME Keyring, KWallet) on Linux. Entries use the service `rustman` and the account `<workspace-id>/<env>/<name>`.
- **Workspace id:** `.rustman/workspace-id` holds a UUID created the first time a secret is stored in the keychain. It's per clone and git-ignored, so two clones of the same repository on one machine never share secrets.
- **File fallback:** when no keychain is available (for example on a headless Linux host with no Secret Service running), secrets go to `.rustman/secrets.yaml`, and the warning `OS keychain unavailable; storing secrets in .rustman/secrets.yaml` is printed once. On Unix the file has mode `0600` (its folder, when Rustman creates it, `0700`). On Windows, inherited permissions and the broad groups (Everyone, Authenticated Users, Users) are removed and the current user is granted full control (with the built-in `icacls`); `SYSTEM` and `Administrators` may keep access, as root does on Unix. The file is replaced atomically on every write. If its permissions were loosened (say, by a copy or a restore), reading a secret from it prints `warning: .rustman/secrets.yaml is readable by other users; ...` once, and the next `rustman secret set` restores mode `0600`.
- **Git-ignore guard:** before writing `.rustman/secrets.yaml` or `.rustman/workspace-id`, Rustman asks Git whether the path is ignored. If the workspace is in a Git repository and `.rustman/` is not ignored (or the file is tracked), the write is refused with `error: refusing to write secrets: .rustman/ is not git-ignored`. `rustman init` adds `.rustman/` to `.gitignore`. Without `git`, or when Git reports that the folder is not in a repository, the check passes; any other check Git can't answer (inside a repository, or when Git fails for another reason, such as an unsafe repository owner) is a refusal with `error: refusing to write secrets: could not check that .rustman/ is git-ignored`.
- **No symbolic links:** a `.rustman` that is a symbolic link (or a link in place of the secrets file) could send secrets into a tracked folder or out of the workspace, so the write is refused with `error: refusing to write secrets: .rustman is a symbolic link`.
- **Choosing the store:** set `RUSTMAN_SECRET_STORE=file` to use only `.rustman/secrets.yaml` and never touch the keychain (useful in CI and containers). `RUSTMAN_SECRET_STORE=keychain`, or leaving it unset, means the keychain with the file fallback.

When a value exists in both places, the keychain wins. Storing a secret in the keychain removes any old copy from the file.

### Redaction

Secret values never appear in any artifact Rustman produces: terminal output, logs, history, reports, copy-as-curl and error messages. They're replaced by `«redacted:<name>»`, for example `«redacted:api_token»`. The values of the `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie` headers are always replaced too, even when they don't come from a secret:

```text
Authorization: «redacted:Authorization»
```

Rustman tracks which bytes of a request came from a secret, and also scans every output for the secret values themselves. That second check catches a server that echoes a secret back in its response, as is or in the form a common encoder writes it. It covers the secrets the requests of the command reference (every request of a `rustman run`, from its first request on, whether or not that request sends them; the one request of `rustman send` or `history replay`), resolved before anything is sent, plus captured values that became secrets (see below). An environment secret no request of the command references is not read from the store, so it is not recognized if a server happens to send it. The forms matched:

- JSON string escapes (`\"`, `\\`, control characters), each with or without `\/` for `/` (PHP), with or without `\u0026`, `\u003c` and `\u003e` for `&`, `<` and `>` and `\u2028` and `\u2029` for U+2028 and U+2029 (Go), and with non-ASCII characters as is or as `\uXXXX` escapes in upper- or lowercase hex (Python, PHP);
- Rust `Debug` escapes;
- percent-encoding in upper- or lowercase hex: every byte but `A-Z a-z 0-9 - . _ ~` encoded, with a space as `%20` or as `+` (form encoding); the same leaving `/` as is (Python's `quote`); or leaving `!'()*` as is (JavaScript's `encodeURIComponent`);
- HTML escapes of `&`, `<` and `>` as `&amp;`, `&lt;` and `&gt;`, with `"` and `'` as `&quot;` and `&#x27;` (Python), `&quot;` and `&#39;`, or `&#34;` and `&#39;` (Go); or every character but letters and digits as a decimal entity such as `&#47;`.

A secret value longer than 8 KiB is redacted in output only as is, not in these encoded forms: building a matcher over the encoded forms of very long values would take memory out of proportion. Captures are still checked for it through decoding (below), so a captured echo of it in any of these forms is still treated as a secret: never persisted, and redacted wherever a later request uses it.

Not covered: Java's `URLEncoder` and PHP's `urlencode` (which keep `*` and encode `~`), XML escapes (`&apos;`), hexadecimal HTML entities other than `&#x27;`, and Go's `\u0008` and `\u000c` for backspace and form feed.

Redaction happens before anything is quoted or shortened: an assertion message that quotes 200 characters of a body, or the 64 KiB preview of a large body, never ends with the start of a secret, and never splits a `«redacted:<name>»` placeholder.

Two kinds of files are working state rather than artifacts, and hold what the server sent as is: the cookie jar (`.rustman/cookies/`), which keeps a cookie whose value echoes a secret so it can be sent back, and a large response body spilled to `.rustman/bodies/` while history keeps its entry. Both are readable by the current user only and live in the git-ignored `.rustman/` folder.

A secret the server transforms before echoing it (base64-encodes it, changes its case, hashes it, splits it) is not recognized, and is shown as the server sent it. Only the forms listed above are matched.

A captured value that holds any piece of a known secret at least 4 characters long (a capture that straddles the secret, or a masked echo such as `****wP3K`) is treated as a secret for the rest of the run and never persisted (see [Captures](assertions.md#captures)). The piece counts as is, or once the captured value is percent-decoded (`+` as a space or as is), JSON-unescaped (`\uXXXX`, `\/`, `\"`, `\\`...) or HTML-unescaped (named and numeric entities), so `%2Fc%2Fd%2Fe%2F` reveals `a/b/c/d/e/f/g`; invalid sequences are left as they are. Pieces shorter than 4 characters are too common to tell apart from other text and are not recognized.

Short secrets may also redact unrelated text: every occurrence of the value is replaced wherever it appears, so a 4-digit PIN `1234` also turns `"duration_ms":1234` in a report into `"duration_ms":«redacted:pin»`, and a secret shorter than 4 characters is matched in almost any text. `rustman secret set` warns when it stores a value shorter than 8 characters.

The only exception is an explicit, per-command opt-in, `--show-secrets`, which affects terminal output only: history, reports and logs stay redacted. `rustman send` and `rustman history replay` accept it to print the response headers and body as received, and `rustman export curl` accepts it to print the real values in the command. Without it, `send` and `replay` print the response redacted, a large body streamed from its spill file too:

| Output | Body |
|--------|------|
| default (status, headers, body) | redacted |
| `--body-only` | redacted |
| `--show-secrets` (with or without `--body-only`) | as received |
| `-o <file>` | as received |

A binary body (any body that is not valid UTF-8, such as an image, or JSON in Latin-1) is redacted byte by byte like a text one: every other byte is printed as received. `-o` writes the file you named byte for byte, as `curl -o` does: it is how to save a body exactly (an image, say), so treat that file like the response itself.

An end-to-end test (`crates/rustman-cli/tests/e2e.rs`) checks this contract: it runs a workspace through `init`, `secret set`, `run` with the json and junit reporters and `-vvv` logs, `export curl`, a failing request, and `history list`, `show` and `replay`, then searches every file it produced and everything it printed for the secret, and finds nothing. Its secret is non-ASCII and contains a `"`, and the server also echoes it JSON-escaped, in a failing assertion message cut mid-secret and across the cut of a spilled body's preview.

## The CI pattern

CI systems keep secrets in their own secret variables, and runners usually have no OS keychain. Give CI its own environment (`environments/ci.yaml` above) that declares the same secrets, and feed each one from a CI secret variable into the file store at the start of the job:

```console
$ printf '%s' "$API_TOKEN" | RUSTMAN_SECRET_STORE=file rustman secret set api_token --env ci
stored secret 'api_token' for environment 'ci' in .rustman/secrets.yaml
$ RUSTMAN_SECRET_STORE=file rustman secret list --env ci
ci/api_token  set
```

Then select the environment with `RUSTMAN_ENV=ci` (or `--env ci`) and keep `RUSTMAN_SECRET_STORE=file` set for the rest of the job. Because `api_token` stays a declared secret, it is redacted from reports and logs like any other secret. `printf '%s'` keeps the value off the command line, so it doesn't show up in the process list or in the job log.

For example, in GitHub Actions:

```text
env:
  RUSTMAN_ENV: ci
  RUSTMAN_SECRET_STORE: file
steps:
  - run: printf '%s' "$API_TOKEN" | rustman secret set api_token
    env:
      API_TOKEN: ${{ secrets.API_TOKEN }}
```

A request can also read an OS environment variable directly with `{{$env NAME}}`. Such values are not secrets, so only the sensitive headers are redacted. Prefer a declared secret for credentials.

Clean up at the end of a job on a shared runner:

```console
$ RUSTMAN_SECRET_STORE=file rustman secret rm api_token --env ci
removed secret 'api_token' from environment 'ci'
```
