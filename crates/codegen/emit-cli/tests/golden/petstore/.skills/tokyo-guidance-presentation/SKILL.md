---
name: tokyo-guidance-presentation
description: Customizes agent guidance and human presentation in generated Tokyo CLI projects without breaking machine protocols. Use when editing guidance.rs or presentation.rs, adding command-specific usage notes, changing help text, styles, banners, command ordering, or when the user mentions agent guidance, clap presentation, branding, JSON output compatibility, or generated command renaming.
---
# Tokyo Guidance and Presentation

## Add agent guidance

Edit `src/commands/guidance.rs`; it is scaffolded once and remains user-owned:

```rust
pub fn command_guidance() -> &'static [(&'static str, &'static str)] {
    &[(
        "orders.create",
        "Use the active caller organization; do not send org_id explicitly.",
    )]
}

pub fn cli_guidance() -> Option<&'static str> {
    Some("Prefer achieve for creation outcomes; use direct commands for reads.")
}
```

Use stable IDs from `<cli> schema`. Overall guidance appears in `start` and root help. Command guidance appears in `schema --command <ID>`, so agents receive it during normal discovery rather than through an extra lookup. Keep notes short, operational, and specific to this product. Use executable scenarios for multi-step workflows instead of prose recipes.

## Customize human presentation

Edit `src/presentation.rs`; the complete clap tree passes through `present` before parsing:

```rust
pub fn present(command: clap::Command) -> clap::Command {
    command
        .about("My product CLI")
        .help_template("{about}\n\nUSAGE: {usage}\n\n{all-args}")
        .styles(clap::builder::Styles::styled())
}
```

Help templates, colors, styles, banners, about text, argument help, and ordering are safe presentation concerns.

## Preserve contracts

- Do not rename or alias generated top-level commands. Dispatch classifies them by generated name before presentation, so renaming can break routing.
- Human help, colors, and table rendering must not alter `--output json` or `json-raw`.
- Machine JSON shape, structured stderr errors, and exit codes are scripting and agent contracts.
- Do not add normal progress or branding output to stdout for machine, streaming, or binary commands.
- Both files survive regeneration; never copy these changes into `.tokyo/**`.
