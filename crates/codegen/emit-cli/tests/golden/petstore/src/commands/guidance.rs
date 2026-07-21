//! Developer-owned agent guidance.
//!
//! Tokyo does not overwrite this file after the initial scaffold. Notes you
//! record here are surfaced to agents inside responses they already load:
//! `cli_guidance` appears in `start` and root `--help`, and `command_guidance`
//! entries appear in `schema --command <ID>` detail for the matching command.

/// Per-command usage notes keyed by stable command ID
/// (for example `("orders.create", "org_id is inferred from your token")`).
pub fn command_guidance() -> &'static [(&'static str, &'static str)] {
    &[(
        "default.create-pet",
        "Prefer `achieve create pet` over calling this directly; it fills defaults for you.",
    )]
}

/// One-paragraph opinion on how an agent should use this CLI overall.
pub fn cli_guidance() -> Option<&'static str> {
    Some(
        "Start with `achieve` for outcomes; fall back to direct commands only when achieve reports an unsupported goal.",
    )
}
