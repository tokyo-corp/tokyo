//! The generated CLI project's own `Cargo.toml` and `README.md` — these
//! need real per-API interpolation, unlike the other templates, so they're
//! plain functions rather than static string constants.

pub fn render_generated_cli_cargo_manifest_source_file(generated_cli_product_name: &str) -> String {
    let cli_runtime_crate_version = env!("CARGO_PKG_VERSION");
    format!(
        r#"[workspace]

[package]
name = {generated_cli_product_name:?}
version = "0.1.0"
edition = "2024"

[[bin]]
name = {generated_cli_product_name:?}
path = ".tokyo/src/main.rs"

[dependencies]
chrono = {{ version = "0.4.42", features = ["serde"] }}
clap = {{ version = "4.6.1", features = ["derive", "env"] }}
clap_complete = "4.6.1"
tokyo-cli-runtime = "={cli_runtime_crate_version}"
serde = {{ version = "1.0.228", features = ["derive"] }}
serde_json = "1.0.150"
uuid = {{ version = "1.18.1", features = ["serde"] }}
"#
    )
}

/// Renders the generated CLI project's starter GitHub Actions release
/// workflow: cross-compiled binaries attached to a GitHub Release on every
/// `v*` tag push, plus an opt-in `cargo publish` job that stays a no-op until
/// the project owner flips the `PUBLISH_TO_CRATES_IO` repository variable on.
pub fn render_generated_cli_release_workflow_source_file(
    generated_cli_product_name: &str,
) -> String {
    format!(
        r#"name: Release

on:
  push:
    tags:
      - "v*"

concurrency:
  group: release-${{{{ github.ref }}}}
  cancel-in-progress: false

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  validate:
    name: Validate release
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy,rustfmt
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets --all-features --locked -- -D warnings
      - run: cargo test --locked

  # Off by default: publishing a binary CLI to crates.io is optional, and
  # `cargo publish` needs a registry token this project doesn't have yet.
  # To turn it on, set the repository variable PUBLISH_TO_CRATES_IO=true
  # (Settings -> Secrets and variables -> Actions -> Variables) and enable
  # Trusted Publishing for this crate at https://crates.io/crates/{generated_cli_product_name}/settings.
  publish-crate:
    name: Publish to crates.io
    needs: validate
    if: vars.PUBLISH_TO_CRATES_IO == 'true'
    runs-on: ubuntu-latest
    environment: release
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: rust-lang/crates-io-auth-action@v1
        id: auth
      - run: cargo publish --locked
        env:
          CARGO_REGISTRY_TOKEN: ${{{{ steps.auth.outputs.token }}}}

  build-binaries:
    name: Build ${{{{ matrix.target }}}}
    needs: validate
    strategy:
      fail-fast: false
      matrix:
        include:
          - runner: ubuntu-24.04
            target: x86_64-unknown-linux-gnu
            archive: tar
          - runner: ubuntu-24.04-arm
            target: aarch64-unknown-linux-gnu
            archive: tar
          - runner: macos-15-intel
            target: x86_64-apple-darwin
            archive: tar
          - runner: macos-14
            target: aarch64-apple-darwin
            archive: tar
          - runner: windows-2025
            target: x86_64-pc-windows-msvc
            archive: zip
    runs-on: ${{{{ matrix.runner }}}}
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{{{ matrix.target }}}}
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{{{ matrix.target }}}}
      - run: cargo build --release --locked --bin {generated_cli_product_name} --target "${{{{ matrix.target }}}}"
        if: matrix.archive == 'tar'
      - name: Archive Unix binary
        if: matrix.archive == 'tar'
        run: |
          mkdir -p dist
          cp "target/${{{{ matrix.target }}}}/release/{generated_cli_product_name}" dist/
          cp README.md dist/
          [ -f LICENSE ] && cp LICENSE dist/
          tar -czf "{generated_cli_product_name}-${{{{ github.ref_name }}}}-${{{{ matrix.target }}}}.tar.gz" -C dist .
      - name: Build Windows binary
        if: matrix.archive == 'zip'
        shell: pwsh
        run: cargo build --release --locked --bin {generated_cli_product_name} --target "${{{{ matrix.target }}}}"
      - name: Archive Windows binary
        if: matrix.archive == 'zip'
        shell: pwsh
        run: |
          New-Item -ItemType Directory -Force -Path dist
          Copy-Item "target/${{{{ matrix.target }}}}/release/{generated_cli_product_name}.exe" dist/
          Copy-Item README.md dist/
          if (Test-Path LICENSE) {{ Copy-Item LICENSE dist/ }}
          Compress-Archive -Path dist/* -DestinationPath "{generated_cli_product_name}-$env:GITHUB_REF_NAME-${{{{ matrix.target }}}}.zip"
      - uses: actions/upload-artifact@v4
        with:
          name: {generated_cli_product_name}-${{{{ matrix.target }}}}
          path: {generated_cli_product_name}-${{{{ github.ref_name }}}}-${{{{ matrix.target }}}}.*
          if-no-files-found: error

  github-release:
    name: Publish GitHub release
    needs: build-binaries
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v5
        with:
          pattern: {generated_cli_product_name}-*
          path: artifacts
          merge-multiple: true
      - name: Create checksums
        working-directory: artifacts
        run: sha256sum {generated_cli_product_name}-* > SHA256SUMS
      - name: Create or update release
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          GH_REPO: ${{{{ github.repository }}}}
        run: |
          if gh release view "${{GITHUB_REF_NAME}}" >/dev/null 2>&1; then
            gh release upload "${{GITHUB_REF_NAME}}" artifacts/* --clobber
          else
            gh release create "${{GITHUB_REF_NAME}}" artifacts/* --verify-tag --generate-notes
          fi
"#
    )
}

pub fn render_generated_cli_readme_source_file(
    generated_cli_product_name: &str,
    cli_behavior_extracted_from_openapi_spec: &tokyo_ir::cli_behavior::CliBehavior,
) -> String {
    let environment_variable_prefix = generated_cli_product_name
        .to_uppercase()
        .replace(['-', ' '], "_");
    let first_login_instructions_markdown = cli_behavior_extracted_from_openapi_spec
        .cli_auth
        .iter()
        .next()
        .map(|(security_scheme_name, oauth_provider_config)| {
            let allowed_login_environment_names = match &oauth_provider_config.endpoints {
                tokyo_ir::cli_behavior::OAuthEndpoints::BrowserToken {
                    allowed_environments,
                    ..
                }
                | tokyo_ir::cli_behavior::OAuthEndpoints::Mock {
                    allowed_environments,
                    ..
                }
                | tokyo_ir::cli_behavior::OAuthEndpoints::MockEnvironment {
                    allowed_environments,
                    ..
                } => allowed_environments.as_slice(),
                _ => &[],
            };
            if allowed_login_environment_names.is_empty() {
                return format!(
                    "Before your first authenticated request, log in once:\n\n```sh\n{generated_cli_product_name} auth login --scheme {security_scheme_name}\n```\n\n"
                );
            }
            let login_commands_for_allowed_environments = allowed_login_environment_names
                .iter()
                .map(|allowed_environment_name| {
                    format!(
                        "{generated_cli_product_name} --environment {allowed_environment_name} auth login --scheme {security_scheme_name}"
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Before your first authenticated request, log in for the target environment:\n\n```sh\n{login_commands_for_allowed_environments}\n```\n\n"
            )
        })
        .unwrap_or_default();
    let local_environment_usage_markdown = if cli_behavior_extracted_from_openapi_spec
        .environments
        .contains_key("Local")
    {
        format!(
            "To test a server on `http://localhost:8000`, select the generated Local environment:\n\n```sh\n{generated_cli_product_name} --environment Local <resource> <command> [args]\n```\n\n"
        )
    } else {
        String::new()
    };
    format!(
        r#"# {generated_cli_product_name}

Tokyo CLI application. Add handwritten commands under `src/routes/**`; do not
edit Tokyo-managed files under `.tokyo/**`.

{first_login_instructions_markdown}{local_environment_usage_markdown}## Usage

```sh
{generated_cli_product_name} --base-url https://api.example.com --token "$API_TOKEN" <resource> <command> [args]
```

For local development, keep `tokyo dev` running and execute the stable binary
directly:

```sh
./.tokyo/bin/{generated_cli_product_name} <command> [args]
```

Agents should use this path instead of repeatedly invoking `cargo run`. It
always points at the latest successful development build.

`--base-url`/`--token` can also come from `{environment_variable_prefix}_BASE_URL`/`{environment_variable_prefix}_TOKEN`.
For APIs with multiple named security schemes, repeat `--credential SCHEME=VALUE`,
set `{environment_variable_prefix}_CREDENTIALS` to an equivalent JSON object, or use
`--credential-file FILE`. Basic and OAuth client-credential values use
`USERNAME:PASSWORD` and `CLIENT_ID:CLIENT_SECRET`, respectively. Environment
variables and owner-protected files are preferred over process arguments for secrets.

Use `{generated_cli_product_name} --profile NAME auth login --scheme SCHEME` to enter a
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

Use `{generated_cli_product_name} env list` to inspect the named API environments compiled
into the CLI. Select one for a single invocation with `--environment NAME` or
`{environment_variable_prefix}_ENVIRONMENT=NAME`.

Persist non-secret connection settings alongside a credential profile:

```sh
{generated_cli_product_name} --profile staging profile set --environment Staging
{generated_cli_product_name} --profile local profile set --base-url http://127.0.0.1:8000
{generated_cli_product_name} --profile staging profile show
{generated_cli_product_name} profile list
```

Connection resolution is: explicit `--base-url`, explicit `--environment`,
the active profile's URL or environment, then the generated default URL.
Connection profiles are stored separately from credentials in `profiles.json`.
Run `{generated_cli_product_name} start` for one generated JSON orientation view containing
the active identity and environment, caller-reachable resources, discovered
scenarios, and concrete next commands. Running `{generated_cli_product_name}` with no
arguments invokes the same orientation view, making it the default entry point.

## Generated outcomes

Use `{generated_cli_product_name} achieve create RESOURCE --count N` for create outcomes
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

Use `{generated_cli_product_name} run list` to inspect available scenarios, or
`{generated_cli_product_name} run NAME` to execute one. Drop `*.scenario` files into
`.{generated_cli_product_name}/scenarios` in a project or the CLI's user config
`scenarios` directory; an explicit path also works. `--set KEY=VALUE`,
`@set:KEY`, and `@last:/pointer` can be used as whole arguments or field
values.

Scenario directives make recipes reusable:

```text
# description: Create several example records
# allowed-environments: Development, Local
@repeat {{count}}
items create --field name=example-{{i}}
@let item_id=@last:/id
@collect item_ids=@var:item_id
@end
@output count=@set:count
```

`{{i}}` is zero-based and `{{index}}` is one-based. `@let` preserves a value
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
"#
    )
}
