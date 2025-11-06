# groppy

A fast, multi-threaded git repository updater written in Rust.

## Features

- **Multi-threaded**: Update multiple repositories in parallel with configurable thread count
- **Smart scanning**: Checks specified directories and one level down for git repositories
- **SSH & HTTPS**: Supports both SSH key authentication and HTTPS connections
- **Clean status**: Only updates repositories with clean working trees
- **Visual feedback**: Spinner with progress counter and terminal integration via OSC 9;4
- **Colorful output**: Uses Catppuccin Macchiato color scheme
  - 🔴 Red for repositories with uncommitted changes
  - 🟢 Green for successfully updated repositories
  - Reports number of files changed per update
- **Summary statistics**: Shows total repositories checked, updated, and unclean

## Installation

```bash
cargo install --path .
```

Or build with native CPU optimizations:

```bash
RUSTFLAGS='-C target-cpu=native' cargo build --release
```

## Usage

```bash
# Update repositories in current directory and one level down
groppy .

# Update repositories in specific directories with 8 threads
groppy -j 8 ~/projects ~/work ~/personal

# Use default 4 threads
groppy ~/code
```

## Options

- `-j <threads>`: Number of threads to use (default: 4)

## How It Works

1. Scans provided directories for git repositories
2. Checks each directory one level down for additional repositories
3. For each repository:
   - Checks if working tree is clean
   - If dirty, reports in red and skips
   - If clean, fetches from origin and fast-forwards
   - Reports updates in green with file change count
   - Silent if no updates available
4. Displays summary of checked/updated/unclean repositories

## Requirements

- Rust 2024 edition
- git2-rs with SSH and HTTPS support
- SSH agent for SSH authentication

## License

MIT
