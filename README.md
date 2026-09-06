# Cargo Shepherd

Cargo `target` directory management CLI/TUI


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

