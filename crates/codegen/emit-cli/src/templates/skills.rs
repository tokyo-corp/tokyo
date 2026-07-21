//! Cursor project skills scaffolded into generated Tokyo CLI applications.

use tokyo_codegen_engine::GeneratedFile;

const PROJECT_SKILLS: &[(&str, &str)] = &[
    (
        ".cursor/skills/tokyo-project-layout/SKILL.md",
        include_str!("skills/tokyo-project-layout/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-filesystem-routes/SKILL.md",
        include_str!("skills/tokyo-filesystem-routes/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-agent-discovery/SKILL.md",
        include_str!("skills/tokyo-agent-discovery/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-scripting-protocol/SKILL.md",
        include_str!("skills/tokyo-scripting-protocol/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-auth-profiles/SKILL.md",
        include_str!("skills/tokyo-auth-profiles/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-achieve-outcomes/SKILL.md",
        include_str!("skills/tokyo-achieve-outcomes/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-scenarios-run/SKILL.md",
        include_str!("skills/tokyo-scenarios-run/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-request-bodies/SKILL.md",
        include_str!("skills/tokyo-request-bodies/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-streaming-binary/SKILL.md",
        include_str!("skills/tokyo-streaming-binary/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-openapi-lifecycle/SKILL.md",
        include_str!("skills/tokyo-openapi-lifecycle/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-project-config/SKILL.md",
        include_str!("skills/tokyo-project-config/SKILL.md"),
    ),
    (
        ".cursor/skills/tokyo-guidance-presentation/SKILL.md",
        include_str!("skills/tokyo-guidance-presentation/SKILL.md"),
    ),
];

/// Returns developer-owned Cursor skills emitted only as an initial scaffold.
pub fn project_skill_starter_files() -> Vec<GeneratedFile> {
    PROJECT_SKILLS
        .iter()
        .map(|(relative_path, contents)| GeneratedFile {
            relative_path: (*relative_path).to_string(),
            contents: (*contents).to_string(),
        })
        .collect()
}
