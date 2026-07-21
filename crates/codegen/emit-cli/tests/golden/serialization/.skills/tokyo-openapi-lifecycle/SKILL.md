---
name: tokyo-openapi-lifecycle
description: Manages the deterministic OpenAPI lifecycle for a generated Tokyo CLI project. Use when adding an API specification, checking upstream drift, syncing a vendored snapshot, regenerating typed commands, reviewing API changes, configuring authenticated OpenAPI downloads, or when the user mentions openapi add, check, sync, upstream.json, or tokyo.lock.
---
# Tokyo OpenAPI Lifecycle

## Add an optional API source

Filesystem routes work without OpenAPI. Add a source only when typed API commands are needed:

```sh
tokyo openapi add https://api.example.com/openapi.json
tokyo generate
```

A local path is also accepted. `openapi add` validates the document, writes a normalized vendored snapshot at `openapi/upstream.json`, records source and snapshot settings in `tokyo.toml`, and writes `tokyo.lock`.

If source acquisition needs headers, `[openapi.headers]` maps header names to environment-variable names. Put the secret value only in the environment; it is never stored in the project.

## Detect and accept drift explicitly

CI check:

```sh
tokyo openapi check
```

`check` reacquires the configured source without writing and exits nonzero if the vendored snapshot or lock would change.

Intentional update:

```sh
tokyo openapi sync
tokyo generate
tokyo check
```

`sync` updates `openapi/upstream.json` and `tokyo.lock`; `generate` updates managed Rust output; `tokyo check` verifies generated output without modifying it.

## Preserve determinism

Normal `tokyo generate`, `tokyo check`, and `tokyo dev` read the vendored snapshot and do not fetch the configured URL. Review the snapshot, lock, and generated diff together. Route, middleware, guidance, and presentation files remain user-owned.

Legacy `[openapi].input` and `--input FILE` remain supported, but new projects should use `openapi add` plus explicit `check`/`sync`. Never edit generated OpenAPI commands directly; update the source/snapshot or user-owned extension points and regenerate.
