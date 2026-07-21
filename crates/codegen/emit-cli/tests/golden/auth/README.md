# generated-cli

Tokyo CLI application. Add handwritten commands under `src/routes/**`; do not
edit Tokyo-managed files under `.tokyo/**`.

Before your first authenticated request, log in once:

```sh
generated-cli auth login --scheme bearerAuth
```

## Usage

```sh
generated-cli --base-url https://api.example.com --token "$API_TOKEN" <resource> <command> [args]
```

For local development, keep `tokyo dev` running and execute the stable binary
directly:

```sh
./.tokyo/bin/generated-cli <command> [args]
```

Agents should use this path instead of repeatedly invoking `cargo run`. It
always points at the latest successful development build.

`--base-url`/`--token` can also come from `GENERATED_CLI_BASE_URL`/`GENERATED_CLI_TOKEN`.
For APIs with multiple named security schemes, repeat `--credential SCHEME=VALUE`,
set `GENERATED_CLI_CREDENTIALS` to an equivalent JSON object, or use
`--credential-file FILE`. Basic and OAuth client-credential values use
`USERNAME:PASSWORD` and `CLIENT_ID:CLIENT_SECRET`, respectively. Environment
variables and owner-protected files are preferred over process arguments for secrets.

Use `generated-cli --profile NAME auth login --scheme SCHEME` to enter a
credential interactively without exposing it in argv. `auth logout` and
`auth whoami` accept the same `--scheme` option. Credentials and OAuth access
tokens are stored in the native OS keychain. If no native backend exists, the
CLI prints a warning and uses an owner-only JSON file; authorization failures
from a locked or denied keychain are reported instead of silently falling back.
OAuth client secrets are never written to the cache.

Every generated operation is classified from OpenAPI security as `public`,
`optional`, or `authenticated`. All commands remain discoverable before login;
use `schema --access public`, `schema --access optional`, or
`schema --access authenticated` to filter the index. Command detail preserves
exact OR/AND schemes and OAuth scopes. A protected command with no usable
credential fails before making a request and returns an
`authentication_required` JSON error with a recovery command. Optional-auth
commands prefer an available identity and otherwise run anonymously.

When an OpenAPI security scheme has interactive OAuth configured, `auth login`
uses Authorization Code with PKCE and opens the system browser. Use
`auth login --device` for SSH, containers, and other headless environments.
Run `auth doctor --scheme SCHEME` to validate provider discovery, public-client
support, PKCE, device authorization, scopes, refresh, and loopback callbacks.
The generated CLI delegates PKCE, token exchange, refresh, and RFC 8628 polling
to the Rust `oauth2` library. Access and refresh tokens remain in the native
credential store; refresh occurs automatically shortly before expiry.

## Environments and profiles

Use `generated-cli env list` to inspect the named API environments compiled
into the CLI. Select one for a single invocation with `--environment NAME` or
`GENERATED_CLI_ENVIRONMENT=NAME`.

Persist non-secret connection settings alongside a credential profile:

```sh
generated-cli --profile staging profile set --environment Staging
generated-cli --profile local profile set --base-url http://127.0.0.1:8000
generated-cli --profile staging profile show
generated-cli profile list
```

Connection resolution is: explicit `--base-url`, explicit `--environment`,
the active profile's URL or environment, then the generated default URL.
Connection profiles are stored separately from credentials in `profiles.json`.
Run `generated-cli start` for one generated JSON orientation view containing
the active identity and environment, caller-reachable resources, discovered
scenarios, and concrete next commands. Running `generated-cli` with no
arguments invokes the same orientation view, making it the default entry point.

## Generated outcomes

Use `generated-cli achieve create RESOURCE --count N` for create outcomes
inferred directly from OpenAPI operation names and request schemas. The CLI
selects the canonical create operation, resolves reusable templates and related
resources through caller-reachable read operations, synthesizes schema defaults,
and returns one structured object containing created IDs. Builder responses that
report failed validation are rejected instead of being presented as successful
outcomes. Pass `--set PATH=VALUE` to override an inferred body field or
`--dry-run` to inspect the generated operations and bodies. Non-create outcomes
such as staging accept repeatable `--id ID` arguments.
Builder-backed outcomes also accept `--prompt REQUEST`; this replaces generic
template text with the caller's own natural-language intent. `--submit` runs
the resource's finalize outcome only after the builder reports successful
validation, preserving create-and-submit as one agent action.
Top-level help advertises caller-reachable outcomes before low-level resource
commands so an agent can orient once and execute on its next call.

## Scenarios

Use `generated-cli run list` to inspect available scenarios, or
`generated-cli run NAME` to execute one. Drop `*.scenario` files into
`.generated-cli/scenarios` in a project or the CLI's user config
`scenarios` directory; an explicit path also works. `--set KEY=VALUE`,
`@set:KEY`, and `@last:/pointer` can be used as whole arguments or field
values.

Scenario directives make recipes reusable:

```text
# description: Create several example records
# allowed-environments: Development, Local
@repeat {count}
items create --field name=example-{i}
@let item_id=@last:/id
@collect item_ids=@var:item_id
@end
@output count=@set:count
```

`{i}` is zero-based and `{index}` is one-based. `@let` preserves a value
after later responses replace `@last:`, `@collect` appends values to an output
array, and `@output` sets a final output field. Normal per-command output is
captured while the recipe runs; stdout receives one final JSON object.
`@self:FIELD` resolves safe fields projected by the active auth provider, such
as `@self:org_id`. A matching `--set NAME=VALUE` overrides an `@let NAME=...`
binding, so recipes can derive a sensible dependency while retaining an
explicit caller override.
`--repeat VALUE` and `# repeat: VALUE` are accepted aliases for `@repeat VALUE`;
close any form with `@end`, `--end`, or `# end`.
Add `# usage: ...` to give `start` an executable hint and `# featured: true`
to make that recipe the primary authenticated next step.

## Output

Output defaults to text in a terminal and JSON when stdout is piped. Pass
`-o json` explicitly for stable pretty JSON in scripts and agents;
`-o json-raw` prints compact JSON.
Errors are reported as a JSON object on stderr.
Text responses are printed directly, binary responses are written byte-for-byte,
and SSE/NDJSON streams emit incrementally (one compact JSON value per stream item).
Redirect binary output to a file when needed.

## Request bodies

For non-flattened bodies, choose exactly one of `--body FILE` (`--body -` reads
stdin), `--body-json JSON`, or repeatable `-f/--field path=value`. Dotted field
paths address objects and numeric segments address array indexes. Field values
that parse as JSON literals keep their JSON type; other values are strings.
JSON requests validate the assembled input against the generated type. Text and
binary `--body` inputs send the source bytes unchanged. Complex URL-encoded and
multipart bodies use JSON as their input shape; a multipart property declared
as `format: binary` contains a local file path. The `api` escape hatch accepts
the same three body-input forms.

## Role-aware commands

Configured dispatch commands retain every original operation and add one public
facade. Without `--view`, the CLI revalidates the active profile credential and
matches only identity fields explicitly selected by the generator configuration;
the configured default member is deterministic when no rule matches. Use
`--view NAME` to choose a configured projection without identity lookup. The
`schema --command` detail reports the default, identity rules, views, and exact
member method/path pairs.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General failure (network error, unmapped API error) |
| 2 | Usage error (invalid flags/arguments) |
| 3 | Not found (HTTP 404) |
| 4 | Permission/auth failure (HTTP 401/403, or a missing credential) |
| 5 | Conflict (HTTP 409) |

## Extending this CLI with routes

This directory is a normal Rust application that you own. Tokyo manages only
`.tokyo/**`. Application code and root project files are user-owned after the
initial scaffold:

- `src/routes/**` — add commands with one `pub fn route() -> Route` per file;
  directories become nested command groups;
- `src/middleware.rs` — decorate every filesystem route with shared
  middleware;
- `src/commands/guidance.rs` — record how agents should use this CLI; notes
  appear in `start`, `--help`, and `schema --command` detail;
- `src/presentation.rs` — restyle help, colors, banners, and ordering for the
  whole command tree;
- `.skills/**` — teach coding agents Tokyo's project workflows and your
  application-specific practices.

`src/commands/custom.rs` remains as a user-owned compatibility hook for older
generated projects; new commands should be filesystem routes.

`Cargo.toml` points the application binary at `.tokyo/src/main.rs`; add the
dependencies needed by handwritten routes there.

Regeneration never overwrites user-owned files, and hand-edits to Tokyo-managed
files are detected (via content hashes in `.tokyo/manifest.json`) rather than
silently erased. When the API spec changes, `tokyo update-branch
--validate --push --pr` turns the update into a reviewable pull request that
touches only managed files.
