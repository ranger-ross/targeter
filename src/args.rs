use std::path::PathBuf;

pub struct Args {
    pub root: PathBuf,
}

impl Args {
    pub fn new() -> Self {
        let root = std::env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        Self { root }
    }
}
