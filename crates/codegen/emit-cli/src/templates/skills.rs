//! Agent skill files scaffolded into generated Tokyo CLI applications.

use tokyo_codegen_engine::GeneratedFile;

const PROJECT_SKILLS: &[(&str, &str)] = &[
    (
        ".skills/tokyo-getting-started/SKILL.md",
        include_str!("skills/tokyo-getting-started/SKILL.md"),
    ),
    (
        ".skills/tokyo-project-layout/SKILL.md",
        include_str!("skills/tokyo-project-layout/SKILL.md"),
    ),
    (
        ".skills/tokyo-filesystem-routes/SKILL.md",
        include_str!("skills/tokyo-filesystem-routes/SKILL.md"),
    ),
    (
        ".skills/tokyo-agent-discovery/SKILL.md",
        include_str!("skills/tokyo-agent-discovery/SKILL.md"),
    ),
    (
        ".skills/tokyo-scripting-protocol/SKILL.md",
        include_str!("skills/tokyo-scripting-protocol/SKILL.md"),
    ),
    (
        ".skills/tokyo-auth-profiles/SKILL.md",
        include_str!("skills/tokyo-auth-profiles/SKILL.md"),
    ),
    (
        ".skills/tokyo-achieve-outcomes/SKILL.md",
        include_str!("skills/tokyo-achieve-outcomes/SKILL.md"),
    ),
    (
        ".skills/tokyo-scenarios-run/SKILL.md",
        include_str!("skills/tokyo-scenarios-run/SKILL.md"),
    ),
    (
        ".skills/tokyo-request-bodies/SKILL.md",
        include_str!("skills/tokyo-request-bodies/SKILL.md"),
    ),
    (
        ".skills/tokyo-streaming-binary/SKILL.md",
        include_str!("skills/tokyo-streaming-binary/SKILL.md"),
    ),
    (
        ".skills/tokyo-openapi-lifecycle/SKILL.md",
        include_str!("skills/tokyo-openapi-lifecycle/SKILL.md"),
    ),
    (
        ".skills/tokyo-project-config/SKILL.md",
        include_str!("skills/tokyo-project-config/SKILL.md"),
    ),
    (
        ".skills/tokyo-guidance-presentation/SKILL.md",
        include_str!("skills/tokyo-guidance-presentation/SKILL.md"),
    ),
    (
        ".skills/tokyo-deployment/SKILL.md",
        include_str!("skills/tokyo-deployment/SKILL.md"),
    ),
];

/// Returns developer-owned agent skills emitted only as an initial scaffold.
pub fn project_skill_starter_files() -> Vec<GeneratedFile> {
    PROJECT_SKILLS
        .iter()
        .map(|(relative_path, contents)| GeneratedFile {
            relative_path: (*relative_path).to_string(),
            contents: (*contents).to_string(),
        })
        .collect()
}
