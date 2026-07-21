---
name: tokyo-getting-started
description: Orients an agent working in a freshly scaffolded Tokyo CLI project — what the framework makes possible and how to shape this specific CLI around the app it serves. Use right after `tokyo init`, when starting the first work session in a Tokyo project, or when the user asks what this framework can do, how to get started, or how to make the generated CLI actually useful.
---
# Tokyo Getting Started

## What this framework gives you for free

Tokyo turned an OpenAPI spec into a typed, authenticated, scriptable Rust CLI. Before writing anything, know what's already available:

- **Typed commands from the spec** — every operation is a subcommand with real request/response types. Regeneration is safe and surgical; see `tokyo-project-layout` and `tokyo-openapi-lifecycle`.
- **Custom commands beside generated ones** — `src/routes/**` adds local or composed logic the spec can't express. See `tokyo-filesystem-routes`.
- **Auth handled correctly by default** — OAuth, API keys, profiles, environments, keychain storage, public/optional/authenticated classification. See `tokyo-auth-profiles`.
- **Goal-oriented commands, not just CRUD** — `achieve` infers create/finalize outcomes from the schema. See `tokyo-achieve-outcomes`.
- **Reusable multi-step recipes** — `.scenario` files with variables, loops, and captured output. See `tokyo-scenarios-run`.
- **Agent-native discovery** — `start` and `schema` give any agent (including you, in a future session) one place to orient without scraping `--help`. See `tokyo-agent-discovery`.
- **A real scripting contract** — stable JSON, structured errors, fixed exit codes, independent of human help/table output. See `tokyo-scripting-protocol`.
- **Streaming and binary I/O** — SSE/NDJSON and raw bytes, not just buffered JSON. See `tokyo-streaming-binary`.
- **Guidance and presentation you control** — `src/commands/guidance.rs` and `src/presentation.rs` survive regeneration. See `tokyo-guidance-presentation`.
- **Declarative project config** — `tokyo.toml` for environments, dispatch groups, embedded scenarios. See `tokyo-project-config`.
- **A ready release pipeline** — tag a version, get cross-compiled binaries on a GitHub Release, optionally publish to crates.io. See `tokyo-deployment`.

## Think about the app, not just the spec

A CLI that mechanically mirrors every OpenAPI operation is a worse product than one shaped around what this app's users actually need to do. Before considering the scaffold "done," think hard about:

- **What are the 3-5 things someone using this CLI is actually trying to accomplish?** Encode those as `achieve` outcomes or `cli_dispatch_groups` in `tokyo.toml` instead of leaving the user to chain low-level operations by hand.
- **What repeated, multi-step workflows exist for this product?** Write them as `.scenario` recipes so they're discoverable via `run list`, not tribal knowledge.
- **What would an agent using this CLI get wrong without help?** Write specific, operational notes in `src/commands/guidance.rs` — not generic advice, but this app's actual footguns and conventions (e.g., "org_id comes from the active identity, never pass it explicitly").
- **Does the generated resource/command naming match how this product's users think and talk**, or does it just mirror OpenAPI tag names? Rename via `cli_name`/dispatch groups where it doesn't.
- **Are there local-only conveniences worth adding as filesystem routes** (e.g., a `doctor` command, a combined setup flow) that no API operation alone provides?
- **Is the environment/profile setup right for how this product is actually deployed** — local, staging, prod — or is it still the generic scaffold default?
- **Does `src/presentation.rs` reflect this product's name and voice**, or is it still generic scaffold output?

Treat the first pass through these questions as part of `tokyo init`, not a later polish step — the CLI's usefulness comes from these decisions, not from the code generation alone.
