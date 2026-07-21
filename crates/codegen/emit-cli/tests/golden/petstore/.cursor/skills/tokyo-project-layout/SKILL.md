---
name: tokyo-project-layout
description: Explains ownership and safe editing in a generated Tokyo CLI project. Use when adding files, changing project structure, regenerating code, resolving managed-file conflicts, or when the user mentions src/tokyo, .tokyo, routes, middleware, guidance, presentation, or generated files.
---
# Tokyo Project Layout

## Respect ownership

Edit these user-owned files:

- `src/routes/**`: filesystem commands
- `src/middleware.rs`: middleware for filesystem routes
- `src/commands/guidance.rs`: agent guidance
- `src/presentation.rs`: clap help and styling
- `src/commands/custom.rs`: compatibility hook only; prefer routes for new commands

Do not hand-edit these Tokyo-managed paths:

- `src/tokyo/**`
- `src/cli.rs`
- `src/main.rs`
- `.tokyo/**`

`Cargo.toml` and `tokyo.toml` are project configuration owned by the repository after scaffolding.

## Regenerate safely

After adding, moving, or deleting a route:

```sh
tokyo generate
```

During active development:

```sh
tokyo dev
```

`generate` overwrites or removes only files listed in `.tokyo/manifest.json`. It never touches unlisted files. The manifest stores SHA-256 hashes; if a managed file was edited, generation fails instead of silently erasing the change. Revert or delete that managed file and regenerate, then move durable behavior into a user-owned file.

Use read-only verification when no rewrite is wanted:

```sh
tokyo check
```

## Development rule

Treat the generated project as a normal Rust application with a managed internal namespace. Add product behavior through routes, middleware, guidance, presentation, and configuration. Never patch generated command or type code to make a durable change.
