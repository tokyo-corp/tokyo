---
name: tokyo-deployment
description: Explains how a generated Tokyo CLI project ships releases via its starter GitHub Actions workflow. Use when adding a release process, cutting a version, publishing binaries, enabling crates.io publishing, or when the user mentions .github/workflows/release.yml, git tag, GitHub Release, or cargo publish.
---
# Tokyo Deployment

## Cut a release

`.github/workflows/release.yml` triggers on any tag matching `v*`:

```sh
git tag v0.1.0
git push origin v0.1.0
```

This runs `validate` (fmt, clippy, `cargo test`), then `build-binaries` in parallel for Linux (x86_64/aarch64), macOS (Intel/Apple Silicon), and Windows, then `github-release`, which uploads every archive plus a `SHA256SUMS` checksum file to a GitHub Release for that tag. Re-pushing the same tag after fixing something updates the existing release instead of failing.

## Enable crates.io publishing (optional)

`publish-crate` is defined but does not run by default — it needs a registry token this project doesn't start with. To turn it on:

1. Set the repository variable `PUBLISH_TO_CRATES_IO=true` (Settings -> Secrets and variables -> Actions -> Variables).
2. Enable Trusted Publishing for this crate at `https://crates.io/crates/<package-name>/settings`, pointing it at this repository's `release.yml` workflow.

Once both are set, the next tag push also runs `cargo publish --locked`, gated on `validate` passing first.

## This is your workflow, not a generated one

`.github/workflows/release.yml` is scaffolded once and never touched by `tokyo generate`, `tokyo update-branch`, or any other regeneration — unlike `.tokyo/**`. Add code signing, notarization, a Homebrew tap, additional target triples, or a different trigger entirely; nothing here is managed. The binary and archive names are derived from the `[[bin]]` name in `Cargo.toml`, so renaming the package there is enough to rename release artifacts too.
