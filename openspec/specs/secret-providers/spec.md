# secret-providers Specification

## Purpose
Resolves secrets from external stores at send time through `{{secret:<provider>/<path>[#field]}}` references. Providers are configured per workspace without credentials, values live only in memory, and every resolved value joins the redaction set of `environments`.

## Requirements

### Requirement: Secret reference syntax
A secret reference SHALL be written `{{secret:<provider>/<path>[#<field>]}}`. `<provider>` is the name of a configured provider, `<path>` is passed to that provider, and `#<field>` selects a field from a structured (JSON or YAML) secret. References SHALL be valid anywhere variable interpolation is allowed. A reference without a path SHALL fail to load with `invalid secret reference '<text>'`.

#### Scenario: Reference in a header
- **GIVEN** a header `Authorization: Bearer {{secret:vault/kv/app/stripe#api_key}}`
- **WHEN** the request is sent
- **THEN** the header SHALL carry the `api_key` field of the `kv/app/stripe` secret from provider `vault`

#### Scenario: Malformed reference
- **GIVEN** `{{secret:vault}}` in a URL
- **WHEN** the workspace loads
- **THEN** loading SHALL report `invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)`

### Requirement: Spaces in secret references
`<path>` and `<field>` MAY contain spaces, but not at either end. No part of a reference SHALL contain other whitespace or braces, and `<provider>` SHALL NOT contain spaces.

#### Scenario: Names with spaces
- **GIVEN** a header `Bearer {{secret:op/Eng Team/Stripe Key/token}}`
- **WHEN** the request is sent
- **THEN** provider `op` SHALL be asked for the path `Eng Team/Stripe Key/token`

### Requirement: Provider-backed environment variables
An environment file SHALL allow a variable whose value is `{secret: "<provider>/<path>[#field]"}`. That variable SHALL resolve through the provider at send time, and SHALL behave as a secret for redaction and display. Its value SHALL never be written to any file.

#### Scenario: Environment variable from 1Password
- **GIVEN** `environments/prod.yaml` with `variables: {api_token: {secret: "op/Engineering/Stripe/token"}}`
- **WHEN** a request using `{{api_token}}` is sent with `--env prod`
- **THEN** the value SHALL come from 1Password
- **AND** `environments/prod.yaml` SHALL be unchanged

### Requirement: Resolution at send time only
References SHALL be resolved only when a request is about to be sent, and only for the request being sent. Loading a workspace, listing requests, opening the TUI or rendering a request file SHALL NOT contact any provider. Request files, environment files and saved requests SHALL keep the reference text and never the value.

#### Scenario: Opening the TUI doesn't fetch
- **GIVEN** a workspace with 40 request files that use secret references
- **WHEN** the TUI opens and the user browses requests
- **THEN** no provider SHALL be contacted

#### Scenario: Saving keeps the reference
- **WHEN** the user edits and saves a request that contains `{{secret:vault/kv/app#pw}}`
- **THEN** the file SHALL contain the reference text, not the value

### Requirement: Provider configuration
Providers SHALL be configured as `[secret_providers.<name>]` with `type` and type-specific, non-credential settings (address, region, project, vault name, profile, mount, auth method). Keys that look like credentials (`token`, `password`, `secret`, `secret_id`, `client_secret`, `private_key`) SHALL be rejected with `credentials must not be stored in rustman.toml (<key>)` and exit code 2.

#### Scenario: Valid Vault config
- **GIVEN** `[secret_providers.vault]` with `type = "vault"`, `address = "https://vault.example.com"`, `auth = "oidc"`
- **WHEN** the workspace loads
- **THEN** provider `vault` SHALL be available

#### Scenario: Token in workspace config
- **GIVEN** `[secret_providers.vault]` containing `token = "hvs.abc"`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with the credentials error and exit code 2

#### Scenario: Unknown provider in a reference
- **GIVEN** `{{secret:lastpass/x}}` and no provider named `lastpass`
- **WHEN** the request is sent
- **THEN** it SHALL fail with `unknown secret provider 'lastpass' (configured: <names>)` as a request error

### Requirement: Nested provider settings
The credential check of `rustman.toml` SHALL cover values at any depth, naming a nested key by its dotted path, and SHALL refuse a string value that is a URL with a `user:password@` part. No provider setting SHALL be a table: a nested table SHALL be a configuration error (exit code 2), never ignored.

#### Scenario: Token in a nested table
- **GIVEN** `[secret_providers.vault.auth]` containing `token = "hvs.abc"`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `credentials must not be stored in rustman.toml (auth.token)` and exit code 2

#### Scenario: Credentials in an address
- **GIVEN** `[secret_providers.vault]` with `address = "https://me:hvs.abc@vault.example.com"`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with the credentials error, without the value, and exit code 2

#### Scenario: A nested table is not ignored
- **GIVEN** `[secret_providers.vault.auth]` containing `method = "approle"`
- **WHEN** the configuration loads
- **THEN** loading SHALL fail with `secret provider 'vault': 'auth' must not be a table` and exit code 2

### Requirement: Supported provider types
The system SHALL support these provider `type` values: `keychain`, `env`, `sops`, `onepassword` and `vault` in every build; `aws-secrets-manager` and `aws-ssm` with cargo feature `secrets-aws`; `gcp-secret-manager` with `secrets-gcp`; `azure-key-vault` with `secrets-azure`; and `cmd` from user config only. Release binaries SHALL enable all three cloud features. A type compiled out SHALL fail with `secret provider type '<t>' is not available in this build (feature <f>)`.

#### Scenario: Cloud type in a minimal build
- **GIVEN** a binary built without `secrets-aws` and a provider with `type = "aws-secrets-manager"`
- **WHEN** a reference to it is resolved
- **THEN** it SHALL fail with the not-available error naming feature `secrets-aws`, as an unresolved secret (exit code 3 for `send`, `run` and `secret get` alike)

### Requirement: Keychain and env providers
A `keychain` provider SHALL read entries written by `rustman secret set` (the store from `environments`), with `<path>` as `<env>/<name>`. An `env` provider SHALL read the process environment variable named by `<path>`. A missing entry or variable SHALL be unresolved.

#### Scenario: Process environment in CI
- **GIVEN** `[secret_providers.ci]` with `type = "env"`, and `STRIPE_KEY` set in the CI job
- **WHEN** `{{secret:ci/STRIPE_KEY}}` is resolved
- **THEN** it SHALL resolve to the value of `STRIPE_KEY`

### Requirement: SOPS provider
A `sops` provider SHALL decrypt SOPS-encrypted YAML or JSON files inside the workspace, with `<path>` as the workspace-relative file and `#field` as a dotted key path. It SHALL decrypt age recipients natively, using `SOPS_AGE_KEY_FILE` or the default age key location. PGP recipients SHALL use an installed `gpg`. Paths outside the workspace SHALL be refused.

#### Scenario: age-encrypted file
- **GIVEN** `secrets/prod.sops.yaml` encrypted for the user's age key
- **WHEN** `{{secret:sops/secrets/prod.sops.yaml#db.password}}` is resolved
- **THEN** it SHALL resolve to the decrypted `db.password`

#### Scenario: Path escape
- **WHEN** `{{secret:sops/../other/keys.yaml#k}}` is resolved
- **THEN** it SHALL fail with `sops: path escapes the workspace` as a request error

#### Scenario: PGP without gpg
- **GIVEN** a PGP-only SOPS file and no `gpg` on `PATH`
- **THEN** resolution SHALL fail with `sops: PGP decryption requires gpg, which was not found`

### Requirement: 1Password provider
A `onepassword` provider SHALL resolve `<vault>/<item>/<field>` using the `op` CLI (`op read`) by default, or using a Connect server when `connect_host` is configured, with its token read from `OP_CONNECT_TOKEN`. The `op` binary path SHALL come from `PATH` or the user config, never from the workspace. In both modes a path without a vault, item and field SHALL fail with `onepassword: path '<path>' must be <vault>/<item>/<field>` before anything is run or contacted.

#### Scenario: op CLI
- **GIVEN** a signed-in `op` CLI and provider `op` of type `onepassword`
- **WHEN** `{{secret:op/Engineering/Stripe/token}}` is resolved
- **THEN** it SHALL return the value of `op read op://Engineering/Stripe/token`

#### Scenario: op not signed in
- **WHEN** `op` reports that the user isn't signed in
- **THEN** resolution SHALL fail with `onepassword: not signed in (run 'op signin')`

### Requirement: Vault provider
A `vault` provider SHALL read KV secrets from `address` (or `VAULT_ADDR`), with an optional `namespace`. It SHALL detect KV v1 or v2 per mount and support `?version=<n>` for v2. Auth SHALL be `token` (from `VAULT_TOKEN` or `~/.vault-token`), `approle` (role and secret ids from the environment or the keychain) or `oidc` (a browser login, with the token held in memory for the session).

#### Scenario: KV v2 with version
- **GIVEN** a KV v2 mount `kv`
- **WHEN** `{{secret:vault/kv/app/db?version=3#password}}` is resolved
- **THEN** version 3 of `kv/app/db` SHALL be read and its `password` field returned

#### Scenario: Permission denied
- **WHEN** Vault returns `403` for the path
- **THEN** resolution SHALL fail with `vault: permission denied for kv/app/db`, without including the token

### Requirement: Cloud providers
These providers SHALL use their platform's native credential chain:
- `aws-secrets-manager` (`<name or ARN>[#field]`) and `aws-ssm` (`<parameter path>`, with decryption), using the AWS default chain with optional `profile` and `region`
- `gcp-secret-manager` (`<secret>[@version]`, default `latest`, with `project`), using Application Default Credentials
- `azure-key-vault` (`<name>[/version]`, with `vault`), using `DefaultAzureCredential`

#### Scenario: AWS JSON secret field
- **GIVEN** provider `aws` with `type = "aws-secrets-manager"`, `region = "eu-west-1"`, and a JSON secret `prod/db`
- **WHEN** `{{secret:aws/prod/db#password}}` is resolved
- **THEN** the `password` field of the secret SHALL be returned

#### Scenario: No cloud credentials
- **GIVEN** no credentials are available in the AWS chain
- **THEN** resolution SHALL fail with `aws-secrets-manager: no credentials found in the default chain`

### Requirement: Command provider is user-scoped
A `cmd` provider SHALL run a configured argv (with no shell) and use its trimmed stdout as the value, with `<path>` passed as the final argument. It SHALL be definable only in the user config (`~/.config/rustman/config.toml`). A `cmd` provider in `rustman.toml` SHALL be refused with `cmd secret providers can only be defined in user config` (exit code 2).

#### Scenario: Workspace tries to define a command
- **GIVEN** `rustman.toml` with `[secret_providers.x]` and `type = "cmd"`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with the user-config-only error and exit code 2

#### Scenario: User-defined command
- **GIVEN** user config `[secret_providers.pass]` with `type = "cmd"` and `command = ["pass", "show"]`
- **WHEN** `{{secret:pass/work/api}}` is resolved
- **THEN** `pass show work/api` SHALL run and its trimmed stdout SHALL be used

### Requirement: Fetch timeouts
Each provider fetch SHALL time out after 10 s by default, configurable per provider with `timeout_ms`. A timed-out fetch SHALL be unresolved, with `<provider>: timed out after <n> ms`.

#### Scenario: Slow provider
- **GIVEN** a provider that doesn't answer within 10 s
- **WHEN** a request referencing it is sent
- **THEN** the request SHALL fail as unresolved, with the timeout message

### Requirement: In-memory cache
Resolved values SHALL be cached in process memory only, keyed by provider and path, with a TTL of 300 s by default (`cache_ttl_s`; `0` disables caching). Concurrent requests for the same key SHALL share one fetch. Cached values SHALL NEVER be written to disk, history or swap-backed files that Rustman controls, and SHALL be dropped when the process exits.

#### Scenario: Run reuses a fetched secret
- **GIVEN** `rustman run` with 50 requests that use `{{secret:vault/kv/app#pw}}`
- **WHEN** the run executes within 300 s
- **THEN** Vault SHALL be contacted once

#### Scenario: Nothing on disk
- **WHEN** a run that resolved provider secrets completes
- **THEN** no file under the workspace, `.rustman/` or the user config directory SHALL contain any resolved value

### Requirement: Unresolved secrets are request errors
A reference that can't be resolved (missing secret, missing field, auth failure, timeout, provider error) SHALL prevent that request from being sent. It SHALL be reported as `unresolved secret <provider>/<path>[#field]: <reason>` and counted as a request error (exit code 3 under `cli-runner`). The message SHALL never contain the secret value or any credential.

#### Scenario: Missing field
- **GIVEN** a secret `kv/app/db` without a `user` field
- **WHEN** `{{secret:vault/kv/app/db#user}}` is resolved during `rustman run`
- **THEN** the request SHALL NOT be sent
- **AND** the error `unresolved secret vault/kv/app/db#user: field 'user' not found` SHALL be reported
- **AND** the run SHALL exit with code 3

### Requirement: Redaction of provider values
Every value resolved through a provider SHALL be added to the redaction set from `environments` for the life of the process. It SHALL be displayed as `«redacted:<provider>/<path>[#field]»`, or `«redacted:<variable>»` when it backs an environment variable, in the TUI, logs, history, reports, copy-as-curl, viz events and MCP results.

#### Scenario: Echoing server
- **GIVEN** a value resolved from `{{secret:aws/prod/db#password}}`, and a server that echoes it in the response body
- **WHEN** the exchange is written to history and to a JSON report
- **THEN** neither SHALL contain the value
- **AND** both SHALL contain `«redacted:aws/prod/db#password»`

#### Scenario: Copy as curl
- **GIVEN** a request whose header is `Bearer {{secret:op/Eng/API/token}}`
- **WHEN** the user runs `rustman export curl` or `rustman export snippet` on it, or copies it from the TUI
- **THEN** the output SHALL contain `Bearer «redacted:op/Eng/API/token»`
- **AND** no provider SHALL be contacted

### Requirement: Secret CLI commands
The CLI SHALL provide:
- `rustman secret get <ref>`: prints `••••••••` with the value's length and provider. `--reveal` prints the value only when stdout is a TTY; otherwise it fails with exit code 2.
- `rustman secret test [<provider>]`: checks auth and connectivity, exiting 0 or 3 (an unknown provider name is a failed check, exit code 3).
- `rustman secret list-providers`: lists name, type, auth method, origin (workspace or user) and status, never values.

#### Scenario: Masked by default
- **WHEN** the user runs `rustman secret get vault/kv/app#pw`
- **THEN** the output SHALL be `•••••••• (24 chars, vault)` and exit with code 0

#### Scenario: Reveal into a pipe refused
- **WHEN** the user runs `rustman secret get vault/kv/app#pw --reveal | cat`
- **THEN** it SHALL fail with `--reveal requires an interactive terminal` and exit code 2

#### Scenario: Provider health check
- **GIVEN** a Vault provider whose token has expired
- **WHEN** `rustman secret test vault` runs
- **THEN** it SHALL report `vault: authentication failed` and exit with code 3

### Requirement: Agents never receive secret values
Saved requests and environments that an agent sends MAY use secret references, which are resolved inside the engine. No tool of the agent interface SHALL return a resolved value: request definitions SHALL show the reference text, and results SHALL be redacted. The agent interface SHALL NOT expose `secret get` or `secret test`.

#### Scenario: Agent reads a request file
- **GIVEN** a request whose header is `Bearer {{secret:op/Eng/API/token}}`
- **WHEN** an agent calls the tool that returns the request definition
- **THEN** the header SHALL be returned as `Bearer {{secret:op/Eng/API/token}}`

#### Scenario: Agent sends the request
- **WHEN** an agent sends that request and the server echoes the token
- **THEN** the tool result SHALL contain `«redacted:op/Eng/API/token»` and not the value

### Requirement: Agents cannot write secret references
An agent SHALL NOT write a reference itself. A tool call whose arguments (ad-hoc URL, headers, body, params, frames, variables) contain a `{{secret:...}}` placeholder SHALL be refused with `invalid_input` before anything is fetched or sent. Values inserted from variables or captures SHALL NOT be resolved again.

#### Scenario: Agent writes a reference into an ad-hoc request
- **WHEN** an agent sends an ad-hoc request with the header `Authorization: Bearer {{secret:op/Eng/API/token}}`
- **THEN** the call SHALL fail with `invalid_input` and a message that references are allowed only in saved request and environment files
- **AND** no provider SHALL be contacted and no request SHALL be sent

#### Scenario: A captured value holds a reference
- **GIVEN** a captured value `{{secret:op/Eng/API/token}}`
- **WHEN** a later request uses `{{token}}`
- **THEN** it SHALL send the text `{{secret:op/Eng/API/token}}` as is, without contacting a provider

### Requirement: Plugins and provider secrets
A plugin SHALL receive a provider-resolved value only with a `secrets:read:<name>` grant from `plugins`, where `<name>` is the backed environment variable or the full reference (`<provider>/<path>[#field]`). Without the grant, the plugin SHALL see the redaction placeholder.

#### Scenario: Auth plugin without grant
- **GIVEN** an auth plugin without a grant, and a param `{{secret:vault/kv/app#key}}`
- **WHEN** the plugin runs
- **THEN** it SHALL receive `«redacted:vault/kv/app#key»`
