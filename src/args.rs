use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "cargo-shepherd", about = "Cargo target directory management")]
pub struct Args {
    /// Directory to scan. Defaults to the home directory.
    pub root: Option<PathBuf>,
}

impl Args {
    pub fn parse_args() -> Self {
        let mut argv: Vec<String> = std::env::args().collect();
        if argv.get(1).is_some_and(|a| a == "shepherd") {
            argv.remove(1);
        }
        <Self as Parser>::parse_from(argv)
    }

    pub fn root(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(|| {
            homedir::my_home()
                .ok()
                .flatten()
                .unwrap_or_else(|| PathBuf::from("."))
        })
    }
}
