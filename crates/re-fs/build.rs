//! Embeds the tablet agents in the client, so `re-fs deploy` works from a copied
//! binary (a release download, a .deb). The Nix build sets RE_FSD_ARMV7 and
//! RE_FSD_AARCH64 to the agents' paths; an unset one embeds nothing, and `deploy`
//! then needs --binary for that CPU.
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (var, file) in [("RE_FSD_ARMV7", "re-fsd-armv7"), ("RE_FSD_AARCH64", "re-fsd-aarch64")] {
        println!("cargo:rerun-if-env-changed={var}");
        let bytes = match env::var_os(var) {
            Some(path) => {
                println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
                fs::read(&path).unwrap_or_else(|e| panic!("{var}={}: {e}", PathBuf::from(&path).display()))
            }
            None => Vec::new(),
        };
        fs::write(out.join(file), bytes).unwrap();
    }
}
