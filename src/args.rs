use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "targeter", about = "Find and clean stale build target dirs")]
pub struct Args {
    /// Directory to scan. Defaults to the home directory.
    pub root: Option<PathBuf>,
}

impl Args {
    pub fn root(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(|| {
            homedir::my_home()
                .ok()
                .flatten()
                .unwrap_or_else(|| PathBuf::from("."))
        })
    }
}
