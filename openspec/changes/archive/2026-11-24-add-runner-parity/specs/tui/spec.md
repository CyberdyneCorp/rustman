## ADDED Requirements

### Requirement: Copy as picker
`Alt+C` and the palette action "Copy as…" SHALL open a picker listing the snippet languages from `http-requests`, with the most recently used one first. `Enter` SHALL generate the snippet for the focused request and copy it. Each language SHALL also appear in the palette as "Copy as <language>".

#### Scenario: Copy as Python
- **GIVEN** a focused HTTP request
- **WHEN** the user presses `Alt+C`, types `py req` and presses `Enter`
- **THEN** the python-requests snippet SHALL be copied and the toast `Copied python-requests snippet` SHALL appear

#### Scenario: Unsupported kind
- **GIVEN** a focused `kafka` request
- **WHEN** the user presses `Alt+C`
- **THEN** a toast SHALL read `No snippet export for kind 'kafka'` and no picker SHALL open

### Requirement: Clipboard via OSC 52
Copying SHALL use the OSC 52 escape sequence, which works over SSH and inside tmux. If the terminal doesn't advertise support, or the payload is over 100 KB, the snippet SHALL open in a read-only pane with the hint `Select to copy`.

#### Scenario: Over SSH
- **GIVEN** Rustman runs over SSH in a terminal supporting OSC 52
- **WHEN** a snippet is copied
- **THEN** the local machine's clipboard SHALL contain the snippet

#### Scenario: Unsupported terminal
- **GIVEN** a terminal without OSC 52 support
- **WHEN** a snippet is copied
- **THEN** the read-only snippet pane SHALL open instead

### Requirement: Secrets in copied snippets
Copied snippets SHALL be redacted by default. While secrets are revealed with `Alt+R`, the picker SHALL show a warning row `Secrets revealed: snippet will contain secret values`, and SHALL require pressing `Enter` twice to copy.

#### Scenario: Copy while revealed
- **GIVEN** secrets are revealed
- **WHEN** the user selects a language and presses `Enter` once
- **THEN** nothing SHALL be copied until a second `Enter` confirms
