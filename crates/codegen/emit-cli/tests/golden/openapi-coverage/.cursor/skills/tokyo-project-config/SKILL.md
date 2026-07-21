---
name: tokyo-project-config
description: Edits and validates tokyo.toml configuration for generated Tokyo CLI projects. Use when changing the project name or routes directory, configuring base URLs or named environments, adding CLI auth providers, embedded scenarios, dispatch groups, OpenAPI source settings, or when the user mentions tokyo.toml, project config, environments, or base_url.
---
# Tokyo Project Configuration

## Use `tokyo.toml`

Tokyo searches the current directory and then ancestors for `tokyo.toml`. Paths are resolved relative to that file.

Minimal route-only project:

```toml
[project]
name = "my-cli"
routes = "src/routes"
```

`[project].routes` must be a relative path containing no `..`. Route discovery defaults to `src/routes`.

## Configure generated API behavior

Top-level generation settings include:

```toml
package = "my-cli"
cli_name = "my-cli"
base_url = "https://api.example.com"

[environments]
Development = "https://api.dev.example.com"
Production = "https://api.example.com"
```

The generated CLI resolves connections in this order: explicit `--base-url`, explicit `--environment`, active profile URL/environment, then `base_url`. Profile base URLs must be absolute HTTP(S) URLs without embedded credentials.

OpenAPI vendoring records:

```toml
[openapi]
source = "https://api.example.com/openapi.json"
snapshot = "openapi/upstream.json"

[openapi.headers]
Authorization = "OPENAPI_AUTHORIZATION"
```

Header values are environment-variable names, not secret values.

## Configure advanced behavior narrowly

- `[cli_auth.<OpenAPI-scheme>]` configures acquisition for that exact security-scheme name.
- `[[cli_scenarios]]` embeds a named recipe; set exactly one of `body` or `file`, and reference only configured environments in `allowed_environments`.
- `[[cli_dispatch_groups]]` creates a public facade over configured operation members while preserving original operations.
- `[sdk]` is reserved configuration; this repository currently emits CLI
  projects, so do not assume it creates an SDK.

Unknown fields are rejected. Prefer explicit, reviewable configuration over patching generated Rust. After changes, run:

```sh
tokyo generate
tokyo check
```

Use `tokyo dev` while iterating. Keep credentials and OAuth client secrets in environment variables, profile storage, or credential files—not in `tokyo.toml`.
