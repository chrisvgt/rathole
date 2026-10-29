use anyhow::Result;
use vergen_gitcl::{Build, Cargo, Emitter, Gitcl};

fn main() -> Result<()> {
    // Only append a "-dirty" suffix for local/developer builds. Skip it in CI
    // or when building a tag so release artifacts remain reproducible and clean.
    let is_ci = std::env::var("CI").is_ok() || std::env::var("GITHUB_ACTIONS").is_ok();
    let is_tag = std::env::var("GITHUB_REF").is_ok_and(|r| r.starts_with("refs/tags/"));
    let dirty = !is_ci && !is_tag;

    let build = Build::builder().build_timestamp(true).build();
    let cargo = Cargo::builder()
        .target_triple(true)
        .features(true)
        .opt_level(true)
        .build();
    let git = Gitcl::builder()
        .sha(false)
        .branch(true)
        .commit_timestamp(true)
        .describe(true, dirty, None)
        .build();

    // Outside a git checkout (e.g. a source archive) vergen leaves the git
    // entries out with a warning, and `--version` falls back to the crate version.
    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&cargo)?
        .add_instructions(&git)?
        .emit()?;
    Ok(())
}
