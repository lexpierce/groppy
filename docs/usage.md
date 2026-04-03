# Usage

Scans directories for git repos and pulls updates in parallel.

## CLI Flags

| Flag | Description | Default |
|------|-------------|---------|
| `[directory...]` | Directories to scan | `.` |
| `-v`, `--verbose` | Verbose output | off |
| `-j N`, `--jobs N` | Parallel job count | `4` |
| `-V`, `--version` | Print version and exit | — |

## Build & Run

```bash
cargo build --release
./target/release/groppy
```

## Examples

```bash
./target/release/groppy                              # Current directory
./target/release/groppy ~/Source                     # Single directory
./target/release/groppy ~/Source ~/Projects ~/Work   # Multiple directories
./target/release/groppy -v -j 8 ~/Source             # Verbose, 8 jobs
```
