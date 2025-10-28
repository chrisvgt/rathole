use anyhow::Result;
use vergen::{vergen, Config};
#[cfg(all(feature = "git", not(windows)))]
use vergen::SemverKind;

fn main() -> Result<()> {
    let mut config = Config::default();
    // When the vergen `git` feature is enabled (non-Windows per Cargo.toml),
    // tweak git-derived semver as before. On Windows, the `git` feature is
    // disabled to avoid libgit2 linking, so this block is not compiled.
    #[cfg(all(feature = "git", not(windows)))]
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
