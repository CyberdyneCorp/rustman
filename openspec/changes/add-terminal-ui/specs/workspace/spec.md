# Spec Delta

## ADDED Requirements

### Requirement: Disabled request rows
A `query`, `headers` or form `fields` row in a request file, or a `headers` row in `_collection.yaml`, MAY carry `enabled: false`. A disabled row SHALL be kept in the file when it is loaded and saved again, SHALL NOT be sent, and SHALL NOT replace a header of the same name inherited from a collection. Rows without the key SHALL be enabled, and saving SHALL write the key only for disabled rows.

#### Scenario: Disabled header kept but not sent
- **GIVEN** a request file with the header rows `X-Debug: 1` (`enabled: false`) and `Accept: */*`
- **WHEN** the request is sent and the file is saved again
- **THEN** only `Accept` SHALL be sent
- **AND** the saved file SHALL still contain `X-Debug` with `enabled: false`

#### Scenario: Disabled header does not override
- **GIVEN** a collection header `X-Team: core` and a request header `X-Team: mine` with `enabled: false`
- **WHEN** the request is sent
- **THEN** `X-Team: core` SHALL be sent
