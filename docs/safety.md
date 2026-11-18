# Safety policy

Rustman can send requests on its own: OpenAPI exploration sends probes, and later the agent interface, load tests, monitors and plugins send requests nobody typed. Every such request goes through one **safety policy** first. It decides, for each request and for every redirect hop, whether the request may leave the machine. The policy is safe by default: read-only, the target's origin only, and never destructive against production.

Requests you send yourself with `rustman send` and `rustman run` are not governed by this policy.

The policy, the rate limit and the budgets are defined in `rustman-core` (`rustman_core::safety`); the sender that enforces them over HTTP is `rustman_http::GovernedSender` (see [For developers](#for-developers)). The `[safety]` example below is checked by a test (`crates/rustman-core/tests/docs_safety.rs`). The command-line flags named on this page belong to the commands that use the policy: [`rustman explore plan`, `explore run` and `explore resume`](exploration.md).

## Methods

| Method | Allowed with |
|--------|--------------|
| `GET`, `HEAD`, `OPTIONS` | always |
| `POST`, `PUT`, `PATCH` | `--allow-mutations` |
| `DELETE` | `--allow-destructive` (which implies `--allow-mutations`) |
| any other method | `--allow-destructive` |

Methods are case-sensitive, as in HTTP. A method Rustman can't classify, such as `TRACE`, `PURGE` or a lowercase `get`, is treated as destructive, because what it does can't be assumed.

A request that isn't sent is recorded, never silently dropped, with one of these reasons:

| Reason | Meaning |
|--------|---------|
| `blocked: mutations not allowed` | `POST`, `PUT` or `PATCH` without `--allow-mutations` |
| `blocked: destructive not allowed` | `DELETE` (or an unclassified method) without `--allow-destructive` |
| `blocked: host not allowed` | the URL's origin isn't allowed, or it isn't an `http`/`https` URL |
| `blocked: production host not confirmed` | an allowed origin whose host is listed in `production_hosts`, but `--production` didn't name it |

## Origins

An **origin** is a scheme, host and port: `https://api.example.com` is `https://api.example.com:443`, and differs from `http://api.example.com` and from `https://api.example.com:8443`. Hosts compare ignoring case and a trailing dot: `https://api.example.com./` has the same origin as `https://api.example.com/`, so the fully qualified spelling of a host gets past neither the allowlist nor the production guard.

The target's origin is always allowed. Every other origin is refused with `blocked: host not allowed`, including redirect targets and URLs built from response data, unless it is listed in `[safety].allowed_hosts` or given with `--allow-host <origin>`. Redirects are followed by hand so that each hop is checked before it is requested.

Allowed origins are written as `scheme://host[:port]`. A bare host, a path, a query or credentials are rejected so an entry can't look narrower than it is:

```text
rustman.toml:6: invalid origin 'auth.example.com': expected scheme://host[:port], such as https://api.example.com
```

## Production guard

A target is **production** when any of these holds:

- its environment file sets `production: true` (see [Environments](environments.md#environment-files));
- its environment is named `prod` or `production` (ignoring case);
- its host matches an entry of `[safety].production_hosts`.

Against production, the command is refused (exit code 4) and nothing is sent, unless `--production <host>` repeats the exact target host:

```text
error: refusing to send requests to production host 'api.example.com' (environment 'production' is a production name); repeat the host with --production api.example.com to allow non-destructive requests
```

With `--production api.example.com`, read-only requests (and mutations, with `--allow-mutations`) are sent. `--allow-destructive` is always refused against production, with no override:

```text
error: refusing --allow-destructive against production host 'api.example.com' (environment 'production' is a production name): destructive requests are never sent to production
```

`--production` naming any other host is refused too (`--production 'example.com' does not match the target host 'api.example.com'`). Repeating the host, rather than passing a yes/no flag, is deliberate: the confirmation doesn't carry over to another target when a command is copied from shell history or a script. An IPv6 host may be given with or without brackets.

An allowed origin whose host matches `production_hosts` is refused per request (`blocked: production host not confirmed`) unless it is the confirmed target host, so an allowlist entry can't open a path to production.

## Rate, concurrency and budgets

Governed requests are paced and capped:

| Limit | Default | Flag | Meaning |
|-------|---------|------|---------|
| Rate | 10 requests/second | `--rate` | Requests are spaced evenly (100 ms apart at 10/s), with no bursts. Every redirect hop counts as a request. |
| Concurrency | 4 | `--concurrency` | Probes in flight at once. A probe holds one slot across its redirect hops. |
| Probe budget | 2000 probes | `--max-probes` | A probe is one request and the redirects it follows. |
| Time budget | 15 minutes | `--max-duration` | Counted from the start of execution (after discovery and planning). A request's timeout is cut to the time left. |

`rate` and `concurrency` can also be set in [`[safety]`](#safety-in-rustmantoml). When a budget is reached, nothing more is sent, and every probe left is recorded as `not run: budget`. A blocked probe isn't sent, so it doesn't count against the probe budget.

### Backing off on 429

A `429 Too Many Requests` response halves the rate, down to at least 1 request per second, for the rest of the session, and nothing is sent until its `Retry-After` has passed. `Retry-After` may be a number of seconds or an HTTP date. With the default rate, `429` with `Retry-After: 5` means no request for 5 seconds, then 5 requests per second. Several `429`s for requests sent at the same rate (for example, the 4 requests in flight at once) halve it only once. A `Retry-After` that ends after the time budget (however large the value) stops execution at once, and the probes left are recorded as `not run: budget`.

## For developers

Front-ends and features that send requests on their own hold only a `GovernedSender` and never an `HttpEngine`, so there is no path around the policy:

```rust
let policy = SafetyPolicy::new(&settings.safety, options)?; // refusals exit with code 4
let governor = Governor::new(Limits::from_settings(&settings.safety));
let sender = GovernedSender::new(policy, governor);
match sender.send(&request, &send_options).await {
    Ok(exchange) => { /* record the exchange */ }
    Err(GovernedError::Blocked { reason, url }) => { /* "blocked: host not allowed" */ }
    Err(GovernedError::NotRun(_)) => { /* "not run: budget": stop dispatching */ }
    Err(GovernedError::Request(error)) => { /* network error, timeout, ... */ }
}
```

`GovernedSender::send` checks the request against the policy, admits it against the budgets and the concurrency limit, waits for the rate limit, and sends it. Each redirect hop is checked and paced again before it is sent; a blocked hop ends the probe with `GovernedError::Blocked` and is never requested. Errors are redacted like the engine's. Create the `Governor` when execution starts, after discovery and planning: the time budget counts from its creation. Requests with `auth: {type: oauth2}` aren't supported yet: the sender acquires no token, so they fail with `oauth2: no access token was acquired` before anything is sent or counted. Waiting for a concurrency slot is unbounded, so an executor should bound the probes it dispatches at once rather than spawn a task per probe for a whole plan.

The rate limit and budgets (`Governor`) live in `rustman-core` and do no I/O, so other governed senders (load tests, monitors, agents) can reuse them for non-HTTP transports.

## `[safety]` in rustman.toml

Every key is optional:

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[safety]
allowed_hosts = ["https://auth.example.com", "http://127.0.0.1:9000"]
production_hosts = ["api.example.com", "*.prod.example.com"]
rate = 10           # requests per second
concurrency = 4     # probes in flight at once
```

| Key | Default | Meaning |
|-----|---------|---------|
| `allowed_hosts` | `[]` | Origins allowed in addition to the target's own; `--allow-host` adds more. |
| `production_hosts` | `[]` | Hosts always treated as production. `api.example.com` matches that host exactly; `*.example.com` matches every subdomain of `example.com`, but not `example.com` itself. Ports and schemes aren't part of an entry. Entries and hosts compare the way URLs normalize them: ignoring case and a trailing dot (`api.example.com.` is `api.example.com`), with internationalized names as punycode (`bücher.example` is `xn--bcher-kva.example`). An IP address is an exact entry; an IPv6 address may be written with or without brackets. Matching is by name only, without DNS: `api.example.com` doesn't match the same server reached by its IP address or through a CNAME alias, so list those too, or mark the environment `production: true`. |
| `rate` | `10` | Requests per second for governed senders; at least 1. `--rate` overrides it. |
| `concurrency` | `4` | Probes in flight at once (a probe and its redirect hops hold one slot); at least 1. `--concurrency` overrides it. |

Unlike other tables, an unknown key in `[safety]` is an error rather than a warning, so a misspelling such as `production_host` can't silently turn the production guard off. Invalid values fail with their line, for example ``rustman.toml:8: invalid value: integer `0`, expected a nonzero u32`` for `rate = 0` in the example above.
