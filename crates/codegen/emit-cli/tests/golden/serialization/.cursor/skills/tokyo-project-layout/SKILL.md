---
name: tokyo-project-layout
description: Explains ownership and safe editing in a generated Tokyo CLI project. Use when adding files, changing project structure, regenerating code, resolving managed-file conflicts, or when the user mentions .tokyo, routes, middleware, guidance, presentation, or generated files.
---
# Tokyo Project Layout

## Respect ownership

Edit these user-owned files:

- `src/routes/**`: filesystem commands
- `src/middleware.rs`: middleware for filesystem routes
- `src/commands/guidance.rs`: agent guidance
- `src/presentation.rs`: clap help and styling
- `src/commands/custom.rs`: compatibility hook only; prefer routes for new commands
- `Cargo.toml`: package metadata, binary declaration, and route dependencies
- `README.md`: project documentation
- `tokyo.toml`: Tokyo project configuration

Do not hand-edit these Tokyo-managed paths:

- `.tokyo/**`

`Cargo.toml` points the package binary at `.tokyo/src/main.rs`. All generated
Rust is disposable output under `.tokyo/src/**`.

## Regenerate safely

After adding, moving, or deleting a route:

```sh
tokyo generate
```

During active development:

```sh
tokyo dev
```

Keep `tokyo dev` running in a background terminal. After it reports a
successful build, test the CLI through its stable executable:

```sh
./.tokyo/bin/<project-name> <command> [args]
```

Do not use `cargo run` for repeated CLI testing. The stable executable avoids
starting Cargo for every command and always points at the latest successful
development build. It has the same environment and credential access as any
other process launched by the agent.

`generate` overwrites or removes only files listed in `.tokyo/manifest.json`. It never touches unlisted files. The manifest stores SHA-256 hashes; if a managed file was edited, generation fails instead of silently erasing the change. Revert or delete that managed file and regenerate, then move durable behavior into a user-owned file.

Use read-only verification when no rewrite is wanted:

```sh
tokyo check
```

## Development rule

Treat the generated project as a normal Rust application with a managed internal namespace. Add product behavior through routes, middleware, guidance, presentation, and configuration. Never patch generated command or type code to make a durable change.
