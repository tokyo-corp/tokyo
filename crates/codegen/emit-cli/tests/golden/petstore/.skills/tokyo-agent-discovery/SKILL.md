---
name: tokyo-agent-discovery
description: Orients agents and discovers generated Tokyo CLI capabilities with start and schema. Use when an agent needs command discovery, stable command IDs, authentication-aware resources, next steps, JSON Schema, command guidance, or when the user mentions start, schema, access filters, or agent discovery.
---
# Tokyo Agent Discovery

## Orient once

Start with:

```sh
<cli> --output json --no-input start
```

Running `<cli>` with no subcommand invokes the same orientation view. `start` reports active identity and environment, reachable resources, authentication status, scenarios, inferred outcomes, and executable next steps. Featured scenarios can become primary next steps.

Prefer `achieve` when `start` advertises a matching user outcome. Inspect low-level operations only when no suitable outcome exists.

## Discover commands cheaply

Load the compact command index:

```sh
<cli> schema
```

Filter by OpenAPI access contract when useful:

```sh
<cli> schema --access public
<cli> schema --access optional
<cli> schema --access authenticated
```

The index uses stable IDs such as `orders.create`. Fetch one command's full contract before invoking it:

```sh
<cli> schema --command orders.create
```

Command detail includes parameters, body mode, authentication alternatives and scopes, exact scripting forms, and any project guidance. Request the transitive request/response schema graph only when needed:

```sh
<cli> schema --command orders.create --json-schema
```

## Agent invariants

- Discovery works without a base URL or login.
- All commands remain discoverable before authentication.
- `src/commands/guidance.rs` supplies overall guidance to `start` and root help, and per-command guidance to `schema --command`.
- Use stable command IDs from `schema`; do not infer flags from operation names.
- Execute automation with explicit `--output json --no-input`.
