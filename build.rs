use anyhow::Result;
use vergen::{vergen, Config};
#[cfg(not(windows))]
use vergen::SemverKind;

fn main() -> Result<()> {
    // Start with a default config and, on non-Windows, tweak git-derived semver.
    let config = {
        let mut c = Config::default();
        #[cfg(not(windows))]
        {
            // Change the SEMVER output to the lightweight variant
            *c.git_mut().semver_kind_mut() = SemverKind::Lightweight;
            // Add a `-dirty` flag to the SEMVER output
            *c.git_mut().semver_dirty_mut() = Some("-dirty");
        }
        c
    };
    // Generate the instructions
    if let Err(e) = vergen(config) {
        eprintln!("error occurred while generating instructions: {:?}", e);
        let config2 = {
            let mut c = Config::default();
            #[cfg(not(windows))]
            {
                *c.git_mut().enabled_mut() = false;
            }
            c
        };
        vergen(config2)
    } else {
        Ok(())
    }
}
