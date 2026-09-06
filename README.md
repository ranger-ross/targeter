# Cargo Shepherd

[![Crates.io](https://img.shields.io/crates/v/cargo-shepherd.svg)](https://crates.io/crates/cargo-shepherd)

Cargo `target` directory management CLI/TUI

<img src="assets/demo.gif" width="800" alt="demo" />

## Features

* Very fast directory discovery
* Management TUI with realtime updates
* Headless mode for listing and cleaning
* `clean` command with reasonable defaults
* Support for `.cargo/config.toml` overrides. (best effort)
* Support for both Cargo `build-dir` and `target-dir`


## Installation

```shell
cargo install cargo-shepherd
```

## Usage

```shell
# Open the management TUI
cargo shepherd

# List the target directories w/o the TUI
cargo shepherd list

# Clean the target directories (defaults to older than 30d AND at least 100MB)
cargo shepherd clean

# Run --help to see more options
cargo shepherd --help
```

