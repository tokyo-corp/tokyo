---
name: tokyo-auth-profiles
description: Configures safe authentication, credentials, environments, and profiles for generated Tokyo CLIs. Use when logging in, selecting a profile or environment, supplying API keys or OAuth credentials, running auth ensure, diagnosing authentication_required, or when the user mentions auth, token, credential, keychain, relay, or whoami.
---
# Tokyo Authentication and Profiles

## Inspect the access contract

OpenAPI operations are `public`, `optional`, or `authenticated`. Root security is inherited unless overridden; `security: []` is public. Security alternatives remain OR, while schemes within one alternative remain AND.

```sh
<cli> schema --command <resource.command>
```

Protected commands fail before the network when credentials are missing. Follow the structured `authentication_required` recovery command.

## Use profiles safely

Store non-secret connection settings:

```sh
<cli> --profile work profile set --environment Development
<cli> --profile work profile show
<cli> profile list
```

Authenticate and inspect identity:

```sh
<cli> --profile work auth login --scheme bearerAuth
<cli> --profile work auth whoami --scheme bearerAuth
<cli> --profile work auth logout --scheme bearerAuth
<cli> auth doctor --scheme bearerAuth
```

Connection precedence is explicit `--base-url`, explicit `--environment`, profile URL/environment, then generated default. An explicit credential overrides a stored profile credential. Profiles store connection data separately from secrets.

## Authenticate agents

Use the idempotent entry point:

```sh
<cli> --output json --no-input auth ensure --interaction forbid
<cli> --output json --no-input auth ensure --interaction relay
```

`forbid` permits no-user-action flows. `relay` emits newline-delimited action objects on stderr while stdout remains the final result; relay the URL/code and let the CLI continue polling. Relay deliberately refuses browser-token mode because passing a reusable bearer token through an agent is secret transfer. Without an explicit policy, `--no-input` implies `forbid`; an interactive terminal implies `allow`.

## Protect secrets

- Prefer login/profile storage, environment variables, or `--credential-file`.
- Do not put reusable secrets in command arguments, logs, source, or `tokyo.toml`.
- The native OS keychain is preferred. If no backend exists, the CLI warns and uses an owner-only file; locked or denied keychains fail rather than silently falling back.
- `auth whoami` exposes only configured/allowlisted identity fields, not arbitrary claims.
- Optional-auth commands use an available identity and otherwise safely run anonymously.
