## ADDED Requirements

### Requirement: Undecoded body limit
A response body that is not decoded (no supported `Content-Encoding`, or `settings.decompress: false`) SHALL fail with `network: the response body is larger than <n> bytes` once it exceeds `rustman.toml [http] max_download_bytes` (default 1 GiB), and its spill file SHALL be removed.

#### Scenario: Endless stream
- **GIVEN** a server that streams a body without end and `max_download_bytes = 2097152`
- **WHEN** the request is sent
- **THEN** the result SHALL be `network: the response body is larger than 2097152 bytes`
- **AND** no spill file SHALL remain

### Requirement: Private spill folder
Outside a workspace, spilled bodies SHALL go to a folder of the current user in the temp folder (`rustman-bodies-<uid>` on Unix). A spill folder that is a symbolic link, or on Unix belongs to another user, SHALL be refused, and a spill file SHALL always be created new, never opened when its path already exists.

#### Scenario: Symbolic link in place of the spill folder
- **GIVEN** the spill folder is a symbolic link to another folder
- **WHEN** a body larger than `max_body_bytes` is received
- **THEN** the request SHALL fail with an error naming the symbolic link
- **AND** nothing SHALL be written to the link's target

### Requirement: Loopback listener limits
The OAuth2 loopback listener SHALL serve at most 16 connections at once, and SHALL wait before retrying a failed `accept`, starting at 10 ms and doubling up to 1 s, instead of retrying at once.

#### Scenario: Accept keeps failing
- **GIVEN** every `accept` on the loopback listener fails
- **WHEN** the login waits 500 ms
- **THEN** `accept` SHALL have been called at most 10 times

### Requirement: Token storage notice
When `rustman auth login` stores the token in `.rustman/secrets.yaml` instead of the OS keychain, it SHALL print, once, `warning: OAuth2 tokens are stored in .rustman/secrets.yaml (git-ignored, readable by you only), not in the OS keychain`.

#### Scenario: File store
- **GIVEN** `RUSTMAN_SECRET_STORE=file`
- **WHEN** `rustman auth login <request>` succeeds
- **THEN** stderr SHALL be exactly that warning line
