# Secret providers

Rustman can read secrets from where your team already keeps them: the OS keychain, environment variables, SOPS files, 1Password, HashiCorp Vault, a command you trust, and (in release builds) AWS, Google Cloud and Azure secret managers. A request names a secret with a **secret reference**. The value is fetched when the request is sent, used on the wire, and never written anywhere.

The behavior is specified in [`openspec/specs/secret-providers`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/secret-providers/spec.md).

## Secret references

```yaml
headers:
  - name: Authorization
    value: Bearer {{secret:vault/kv/app/stripe#api_key}}
```

A reference is `{{secret:<provider>/<path>[#<field>]}}`:

- `<provider>` is the name of a provider configured in `[secret_providers.<name>]`.
- `<path>` is handed to that provider as is. Its meaning depends on the type (a Vault path, a 1Password `vault/item/field`, a SOPS file, an environment variable name...).
- `#<field>` picks a field of a structured secret (JSON or YAML), as a dotted key path: `#db.password`, `#hosts.0`. Strings come out as they are, numbers and booleans as text, objects and arrays as JSON.

The path and field may contain spaces, as 1Password vault and item names often do (`{{secret:op/Eng Team/Stripe Key/api token}}`), but not at either end; the provider name may not. Tabs, line breaks and braces are not allowed anywhere in a reference.

References work anywhere `{{variable}}` interpolation does: URLs, query parameters, headers, bodies, auth fields, and the other request kinds. A reference without a path is reported when the workspace loads:

```text
api.rman.yaml:5: invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)
```

### Environment variables backed by a provider

An environment variable can take its value from a provider:

```yaml
# environments/prod.yaml
variables:
  base_url: https://api.example.com
  api_token: {secret: "op/Engineering/Stripe/token"}
```

`{{api_token}}` then resolves through the `op` provider when a request is sent with `--env prod`. It behaves like any secret: it is redacted as `«redacted:api_token»`, and the environment file keeps the reference. Saving the environment from the TUI writes it back unchanged.

### When values are fetched

Only when a request is about to be sent, and only for that request. Loading the workspace, listing requests, opening the TUI, browsing and rendering request definitions (`rustman export snippet` placeholders, agent tools) never contact a provider. Saved request and environment files always keep the reference text.

All references of one request are fetched together, at most 4 at a time.

## Configuring providers

Providers are declared in `rustman.toml`. The workspace file holds settings only, never credentials: every provider type gets its credentials from its own native chain (environment variables, its CLI's login, files in your home directory).

```toml
[secret_providers.ci]
type = "env"
timeout_ms = 5000
cache_ttl_s = 60
```

Every provider accepts:

| Key | Default | Meaning |
| --- | --- | --- |
| `type` | (required) | One of the types below. |
| `timeout_ms` | `10000` | How long one fetch may take. A fetch that takes longer is unresolved, with `<provider>: timed out after <n> ms`. |
| `cache_ttl_s` | `300` | How long a fetched value is reused, in seconds. `0` disables caching. |

Keys that look like credentials are refused in `rustman.toml`, with exit code 2: `token`, `password`, `secret`, `secret_id`, `client_secret`, `private_key`, and any key whose name contains `token`, `password`, `passwd`, `secret`, `private_key`, `api_key`, `apikey`, `access_key` or `credential` (ignoring case, `-` read as `_`), such as `secret_access_key` or `auth_token`:

```text
error: rustman.toml:4: credentials must not be stored in rustman.toml (token)
```

The check covers nested values too (a credential key inside an inline table or array is reported by its dotted path, such as `auth.token`), and a URL with a `user:password@` part in any string value is refused as `credentials must not be stored in rustman.toml (address: URL with credentials)`. No provider setting is a table: a nested table such as `[secret_providers.vault.auth]` is an error (`secret provider 'vault': 'auth' must not be a table`, exit code 2) rather than being ignored, in `rustman.toml` and in the user config.

Unknown keys only produce a warning, like other `rustman.toml` keys.

### The user config

Your own config file, `config.toml` in the user config directory, can also declare `[secret_providers.*]`:

| Platform | User config file |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/rustman/config.toml`, else `~/.config/rustman/config.toml` |
| macOS | `$XDG_CONFIG_HOME/rustman/config.toml`, else `~/.config/rustman/config.toml` |
| Windows | `%APPDATA%\rustman\config.toml` |

A cloned workspace is untrusted input, so anything that makes Rustman run a program is accepted **only** from the user config: `cmd` providers, the binary paths `op_path` and `gpg_path`, and the cloud providers' `endpoint` (which would otherwise let a workspace send your cloud credentials to a host of its choice). In `rustman.toml` they are refused with exit code 2 (`cmd secret providers can only be defined in user config`, or `'op_path' can only be set in user config`).

For the same reason, a host named in `rustman.toml` never receives credentials Rustman finds on your machine. A Vault `address` from the workspace gets your token (`VAULT_TOKEN`, `~/.vault-token`) or AppRole ids only when `VAULT_ADDR` names the same address or your user config sets `address`; otherwise resolution fails with `vault: address <url> is set in rustman.toml; set VAULT_ADDR to it, or 'address' in user config, to send your Vault credentials there`, an unresolved secret (exit code 3). An OIDC login is allowed, since that token is issued by the server itself. A 1Password `connect_host` works the same way with `OP_CONNECT_HOST` and `OP_CONNECT_TOKEN`. AWS `region` values must be region names (`[a-z0-9-]`), since they become part of the endpoint host.

A user table with a `type` defines a provider (or replaces the workspace's provider of the same name). A user table without one overrides keys of the workspace's provider of that name, which is how you point a shared `onepassword` provider at your own `op` binary:

```toml
# ~/.config/rustman/config.toml
[secret_providers.op]
op_path = "/opt/1password/op"
```

## Provider types

| Type | Built | `<path>` |
| --- | --- | --- |
| `keychain` | always | `<env>/<name>` |
| `env` | always | an environment variable name |
| `sops` | always | a workspace-relative SOPS file |
| `onepassword` | always | `<vault>/<item>/<field>` |
| `vault` | always | `<mount>/<path>[?version=<n>]` |
| `cmd` | always, user config only | passed as the last argument |
| `aws-secrets-manager` | feature `secrets-aws` | a secret name or ARN |
| `aws-ssm` | feature `secrets-aws` | a parameter path |
| `gcp-secret-manager` | feature `secrets-gcp` | `<secret>[@version]` |
| `azure-key-vault` | feature `secrets-azure` | `<name>[/version]` |

Official release binaries enable every cloud feature. A build without one still accepts the configuration, and every reference to it fails with:

```text
secret provider type 'aws-secrets-manager' is not available in this build (feature secrets-aws)
```

### keychain

The secrets `rustman secret set` stores (see [Environments](environments.md#where-secrets-are-stored)), read with `<path>` as `<env>/<name>`. It honors `RUSTMAN_SECRET_STORE` like the rest of Rustman.

```toml
[secret_providers.kc]
type = "keychain"
```

`{{secret:kc/prod/api_token}}` reads what `rustman secret set api_token --env prod` stored.

### env

The process environment: `<path>` is the variable name. This is the usual CI provider.

```toml
[secret_providers.ci]
type = "env"
```

`{{secret:ci/STRIPE_KEY}}` is the value of `STRIPE_KEY`; an unset variable is unresolved.

### sops

SOPS-encrypted YAML or JSON files inside the workspace. `<path>` is the file, relative to the workspace root; `#field` is a dotted key path into the decrypted document. Paths that leave the workspace are refused (`sops: path escapes the workspace`).

```toml
[secret_providers.sops]
type = "sops"
```

`{{secret:sops/secrets/prod.sops.yaml#db.password}}` is the decrypted `db.password`.

- **age** recipients are decrypted natively. Identities come from `SOPS_AGE_KEY` (inline keys), `SOPS_AGE_KEY_FILE`, or SOPS' default key file (`sops/age/keys.txt` in `$XDG_CONFIG_HOME`, `~/Library/Application Support` on macOS, `~/.config` on Linux, `%APPDATA%` on Windows).
- **PGP** recipients are decrypted by running an installed `gpg` (`gpg --batch --decrypt`), found on `PATH` or at `gpg_path` from the user config. Without it: `sops: PGP decryption requires gpg, which was not found`.
- The file's MAC is checked before any value is returned; a modified file fails with `sops: MAC mismatch in <file>`.

### onepassword

1Password items, with `<path>` as `<vault>/<item>/<field>` (or `<vault>/<item>/<section>/<field>`). Names may contain spaces. A path with fewer parts fails with `onepassword: path '<path>' must be <vault>/<item>/<field>` before `op` or Connect is contacted.

```toml
[secret_providers.op]
type = "onepassword"
```

By default the provider runs the [1Password CLI](https://developer.1password.com/docs/cli/): `op read op://<vault>/<item>/<field>`. Sign in once with `op signin` (or the desktop app integration); a signed-out CLI fails with `onepassword: not signed in (run 'op signin')`. `account` selects an account when you have several; `op_path` (user config only) names the binary.

For CI, point the provider at a [1Password Connect](https://developer.1password.com/docs/connect/) server. Its token is read from `OP_CONNECT_TOKEN`, never from the file:

```toml
[secret_providers.op]
type = "onepassword"
connect_host = "https://connect.example.com"
```

### vault

HashiCorp Vault KV secrets. `<path>` starts with the mount; the provider asks Vault whether the mount is KV v1 or v2 (once per mount) and reads `?version=<n>` from KV v2.

```toml
[secret_providers.vault]
type = "vault"
address = "https://vault.example.com"
namespace = "team-a"
auth = "token"
```

`{{secret:vault/kv/app/db?version=3#password}}` reads version 3 of `kv/app/db` and returns its `password` field.

- `address` defaults to `VAULT_ADDR`. `namespace` is sent as `X-Vault-Namespace` (Vault Enterprise and HCP).
- `auth = "token"` (the default) uses `VAULT_TOKEN`, else the `~/.vault-token` file that `vault login` writes.
- `auth = "approle"` logs in once per session with `VAULT_ROLE_ID` and `VAULT_SECRET_ID` from the environment; `mount` changes the auth mount (default `approle`).
- `auth = "oidc"` opens your browser for an OIDC login once per session and keeps the token in memory. `role` picks the Vault role, `mount` the auth mount (default `oidc`), and `oidc_port` the local callback port (default `8250`). The role must allow `http://localhost:8250/oidc/callback` as a redirect URI, as for `vault login -method=oidc`.

```toml
[secret_providers.vault]
type = "vault"
address = "https://vault.example.com"
auth = "oidc"
role = "developer"
```

A `403` is reported as `vault: permission denied for <path>`, without the token.

### cmd

Runs a command you trust and uses its trimmed standard output. The argv runs directly, with no shell, and `<path>` is appended as the last argument. Allowed in the user config only.

```toml
# ~/.config/rustman/config.toml
[secret_providers.pass]
type = "cmd"
command = ["pass", "show"]
```

`{{secret:pass/work/api}}` runs `pass show work/api`. A non-zero exit fails with `pass: command failed (exit status <n>)`; its output is never shown. Since the path comes from the workspace, a path starting with `-` is refused (`pass: path '<path>' must not start with '-'`) so it cannot pass an option to your command. A command that does not finish within the provider's `timeout_ms` is killed; on Linux and macOS so are the programs it started (it runs in a process group of its own, so it cannot prompt on the terminal), while on Windows only the command itself is.

### Cloud providers

The cloud types are thin REST clients on Rustman's own HTTP engine: no cloud SDK and no system library. Each one finds credentials through its platform's documented chain, in the same order as the official SDKs, so whatever already works for the platform's CLI works here. Instance metadata endpoints, which exist only on cloud machines, get 1 s to answer.

`endpoint` (user config only, like every setting that could send credentials elsewhere) points a provider at an emulator such as LocalStack:

```toml
# ~/.config/rustman/config.toml
[secret_providers.localstack]
type = "aws-secrets-manager"
region = "us-east-1"
endpoint = "http://localhost:4566"
```

Release binaries include all three (cargo feature `cloud-secrets` of `rustman-cli`); a source build adds them with `cargo build --features rustman-cli/cloud-secrets`, or one at a time with `secrets-aws`, `secrets-gcp` or `secrets-azure`.

### aws-secrets-manager and aws-ssm

```toml
[secret_providers.aws]
type = "aws-secrets-manager"
region = "eu-west-1"
profile = "prod"

[secret_providers.ssm]
type = "aws-ssm"
region = "eu-west-1"
```

`{{secret:aws/prod/db#password}}` returns the `password` field of the JSON secret `prod/db` (`<path>` is a secret name or ARN; a binary secret must be UTF-8 text). `aws-ssm` reads a parameter with decryption; a reference cannot start its path with `/`, so `{{secret:ssm/app/key}}` reads `/app/key`.

- `region` defaults to `AWS_REGION`, then `AWS_DEFAULT_REGION`. Without one, its references are unresolved with `aws-secrets-manager: no region (set 'region' or AWS_REGION)` (exit code 3).
- Credentials come from the AWS default chain: `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`, the shared profile (`AWS_PROFILE`, else `default`) in `~/.aws/credentials` and `~/.aws/config`, container credentials (ECS, EKS Pod Identity), then the EC2 instance metadata service (IMDSv2; `AWS_EC2_METADATA_DISABLED=true` skips it). With `profile`, only that profile is used. Profiles that need SSO, a role or `credential_process` are refused: export temporary credentials (`aws configure export-credentials --format env`) instead.
- Nothing found: `aws-secrets-manager: no credentials found in the default chain`.
- Requests are signed with SigV4; the endpoint is `https://secretsmanager.<region>.amazonaws.com` (or `ssm.`), or `AWS_ENDPOINT_URL_SECRETS_MANAGER`, `AWS_ENDPOINT_URL_SSM`, `AWS_ENDPOINT_URL`.

### gcp-secret-manager

```toml
[secret_providers.gcp]
type = "gcp-secret-manager"
project = "my-project"
```

`<path>` is `<secret>[@version]` (default `latest`): `{{secret:gcp/db-password@3}}`.

- `project` defaults to `GOOGLE_CLOUD_PROJECT`, then the project of the credentials file.
- Credentials come from Application Default Credentials: the file named by `GOOGLE_APPLICATION_CREDENTIALS` (a service account key), else the file `gcloud auth application-default login` writes, else the metadata server of a Google Cloud machine. Workload identity federation files are refused. Nothing found: `gcp-secret-manager: no credentials found (Application Default Credentials)`.

### azure-key-vault

```toml
[secret_providers.azure]
type = "azure-key-vault"
vault = "my-vault"
```

`<path>` is `<name>[/version]`, read from `https://<vault>.vault.azure.net`.

- Credentials come from `DefaultAzureCredential`'s chain: a service principal from `AZURE_TENANT_ID`, `AZURE_CLIENT_ID` and `AZURE_CLIENT_SECRET`, workload identity (`AZURE_FEDERATED_TOKEN_FILE`), managed identity (App Service, or the instance metadata service), then the Azure CLI (`az login`). Nothing found: `azure-key-vault: no credentials found (DefaultAzureCredential)`.

### Network settings

Provider calls (Vault, 1Password Connect and the cloud types) connect directly and verify servers against the operating system's trust store. The workspace's TLS files (`ca_file`, client certificates), `insecure` and proxy settings, and the `HTTPS_PROXY`/`HTTP_PROXY` environment variables, are not applied to them, so a workspace cannot weaken them. A server behind a private CA needs that CA in the system trust store, and a server reachable only through a proxy cannot be used.

## Caching

Fetched values are kept in process memory only, per provider and path, for `cache_ttl_s` (300 s by default). Requests that need the same secret at the same time share one fetch, so a `rustman run` of 50 requests using `{{secret:vault/kv/app#pw}}` contacts Vault once. Cached values are zeroized when they expire and when the process exits; nothing is ever written to disk. In the TUI, the **Refresh secrets** palette action (`secrets.refresh`) clears the cache so the next send fetches again.

## Failures and exit codes

A reference that cannot be resolved (missing secret or field, failed auth, timeout, provider error, unknown provider) stops its request before anything is sent:

```text
unresolved secret vault/kv/app/db#user: field 'user' not found
unresolved secret lastpass/x: unknown secret provider 'lastpass' (configured: ci, vault)
```

It counts as a request error: `rustman send` and `rustman run` exit with code 3. A misconfigured provider (a credential, a command or a binary path in `rustman.toml`, an unknown type, a missing `type`) is a configuration error, exit code 2, reported when the configuration loads. A problem found only while resolving a reference (a provider type not in this build, a Vault without an address) makes that reference unresolved: exit code 3, in `send`, `run` and `secret get` alike. Messages never contain a secret value or a credential.

## Redaction

Every fetched value joins the redaction set of [environments](environments.md#redaction) for the life of the process. It is shown as `«redacted:<provider>/<path>[#field]»`, or `«redacted:<variable>»` when it backs an environment variable, in terminal output, logs, history, reports, snippets and agent results, including when a server echoes it back.

`rustman export curl`, `rustman export snippet` and the TUI's "Copy as…" never contact a provider: a reference is printed as `«redacted:<provider>/<path>[#field]»` (or `«redacted:<variable>»` for a provider-backed environment variable), even with `--show-secrets`, which reveals keychain secrets only. With `--placeholders`, references stay as written (`{{secret:...}}`).

## The secret commands

```console
$ rustman secret get vault/kv/app#pw
•••••••• (24 chars, vault)
$ rustman secret test
ci: ok
error: vault: authentication failed
$ rustman secret list-providers
NAME   TYPE   AUTH  ORIGIN     STATUS
ci     env    -     workspace  available
vault  vault  oidc  workspace  available
```

- `rustman secret get <ref>` fetches a reference (`<provider>/<path>[#field]`, also accepted as `{{secret:...}}`) and prints its length and provider, never the value. `--reveal` prints the value, and only when standard output is a terminal: `rustman secret get vault/kv/app#pw --reveal | cat` fails with `--reveal requires an interactive terminal` (exit code 2) before contacting the provider. A reference that cannot be resolved exits with code 3, whatever the cause (an unknown provider, or a type not in this build, included), the same code `send` and `run` give it.
- `rustman secret test [<provider>]` checks that each provider (or the one named) can authenticate and be reached, without reading a secret: Vault looks its token up, 1Password runs `op whoami` (or lists Connect vaults), the cloud types obtain a token (AWS reads a secret name that does not exist: a not-found answer means the keys were accepted). It exits with 0 when every check passes and 3 otherwise, an unknown provider name included; use it to fail a CI job early, before a long run.
- `rustman secret list-providers` lists each provider's name, type, auth method, origin (`workspace` or `user`) and whether its type is in this build. It contacts no provider. `--types` lists the provider types this build supports, without a workspace.

## Agents

Agents (`rustman mcp serve`) can send requests that use secret references: they are resolved inside the engine, through the same providers and cache. No agent tool returns a value: `get_request` shows the reference text (`Bearer {{secret:op/Eng/API/token}}`), results show `«redacted:op/Eng/API/token»` even when a server echoes the value, and there is no tool that reads or tests a secret.

Agents cannot write references themselves. A tool call whose arguments contain a `{{secret:...}}` placeholder (an ad-hoc URL, header, body, JSON-RPC params, WebSocket frames, gRPC message or metadata, or `vars`) is refused with `invalid_input` (`secret references ({{secret:...}}) are allowed only in saved request and environment files, not in tool arguments`) before any provider is contacted, so a prompt-injected agent cannot choose which secret is fetched or where it is sent. Values inserted from variables and captures are never resolved again, so a server response that contains `{{secret:...}}` text is sent back as that text.

## Plugins

A value a provider resolved reaches a [plugin](plugins.md) (an auth scheme's parameters, a transformer's message, an assertion's exchange) only when the plugin holds `secrets:read:<name>`, where `<name>` is the environment variable the reference backs, or the reference itself (`secrets:read:vault/kv/app#key`) when the request uses `{{secret:...}}` directly. Without the grant the plugin sees `«redacted:<name>»`. Captured secrets and secret data columns are treated the same way, and everything a plugin returns is redacted of all of them.
