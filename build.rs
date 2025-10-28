use anyhow::Result;
use vergen::{vergen, Config};
#[cfg(not(windows))]
use vergen::SemverKind;

fn main() -> Result<()> {
    let config = Config::default();
    // On non-Windows platforms we enable vergen's `git` feature in Cargo.toml,
    // so we can customize git-derived semver here. On Windows, that feature is
    // disabled to avoid libgit2 linking, so this block is not compiled.
    #[cfg(not(windows))]
    {
        // Change the SEMVER output to the lightweight variant
        *config.git_mut().semver_kind_mut() = SemverKind::Lightweight;
        // Add a `-dirty` flag to the SEMVER output
        *config.git_mut().semver_dirty_mut() = Some("-dirty");
    }
    // Generate the instructions
    if let Err(e) = vergen(config) {
        eprintln!("error occurred while generating instructions: {:?}", e);
        let mut config = Config::default();
        *config.git_mut().enabled_mut() = false;
        vergen(config)
    } else {
        Ok(())
    }
}
