# Development

## Project Layout

```text
.
├── Cargo.toml          # groppy (Rust)
├── Cargo.lock
└── src/main.rs         # source
```

## Tech Stack

Rust, gitoxide (`gix`), reqwest (HTTPS transport), rayon, clap, crossterm, anyhow.

## Build

```bash
cargo build --release
# binary: target/release/groppy
```

## Test

```bash
cargo test
```

## Lint / Format

```bash
cargo fmt
cargo clippy
```

## Dependencies

| Dependency | Pin Strategy | Notes |
|-----------|--------------|-------|
| `gix` | minor (`0.80`) | gitoxide — moves fast, breaking changes between minors. Features: `blocking-network-client`, `blocking-http-transport-reqwest-rust-tls`, `status`, `worktree-mutation` |
| `crossterm` | minor (`0.29`) | Terminal I/O — pre-1.0, breaking changes between minors |
| `clap` | major (`4`) | CLI parser — semver-stable |
| `rayon` | major (`1`) | Parallelism — semver-stable |
| `anyhow` | major (`1`) | Error handling — semver-stable |

### Update Workflow

```bash
cargo update
cargo test
cargo clippy
```

### Pin Strategy

- **Major-version pins** (`"4"`, `"1"`): semver-stable crates. `cargo update` pulls latest compatible.
- **Minor-version pins** (`"0.80"`, `"0.29"`): pre-1.0 crates where minors contain breaking changes. Bump manually after reviewing changelogs.

## Versioning

Version is defined in `Cargo.toml` → `version = "x.y.z"`.

Classify commits since last version tag:

| Prefix | semver impact |
|--------|---------------|
| `feat:` | minor bump |
| `fix:`, `perf:`, `refactor:` | patch bump |
| `BREAKING CHANGE` / `!` | major bump |
| `docs:`, `chore:`, `style:` | no bump |

`-V`/`--version` is auto-wired by clap `#[command(version)]` from `Cargo.toml`. No manual code needed.

## Architecture Notes

### gix Worktree Checkout API

Key facts for implementing worktree updates with gitoxide:

- `repo.index_from_tree(&tree_oid)` — builds a `gix_index::File` from a tree OID (not a commit OID — peel first with `.peel_to_tree()`).
- `gix::worktree::state::checkout(...)` writes index entries to disk. Does **not** delete files absent from the new index — unlink deletions inline in the `for_each_to_obtain_tree` closure.
- Enable path tracking on the diff platform: `.changes()?.options(|o| { o.track_path(); })`. Without it `location` is always empty.
- `for_each_to_obtain_tree` callback return type is `ControlFlow<()>` — use `Action::Continue(())` not `Action::Continue`.
- After checkout, call `index.write(Default::default())?` to persist the index; omitting this leaves git status stale.
- `checkout_result.files_updated` is total files written (full index), **not** the diff delta. Use `for_each_to_obtain_tree` diff walk for accurate `files_changed`.
- Pass `gix::progress::Discard` for the `files` and `bytes` progress params when no progress reporting is needed.

### Ref Mutation Safety

Hold the `Reference` returned by `head_ref()` as `mut` and call `set_target_id` directly — do not `drop` it and re-lookup with `find_reference`.

`set_target_id` uses `PreviousValue::MustExistAndMatch` internally, failing atomically if the ref moved.

```rust
let mut head_ref = repo.head_ref()?.ok_or(...)?;
// ... fetch ...
head_ref.set_target_id(new_id, "groppy: fast-forward")?;
// ... on failure ...
let msg = match head_ref.set_target_id(old_id, "groppy: revert failed fast-forward") {
    Ok(_) => format!("Checkout failed: {e}"),
    Err(revert_err) => format!("Checkout failed: {e}; revert also failed: {revert_err}"),
};
```

## Code Style

### Naming

- `snake_case` for functions/variables, `PascalCase` for types/traits
- Descriptive names, no abbreviations

### Error Handling

- Use `anyhow::Result`, propagate with `?`
- Handle at appropriate level, don't swallow
- Post-success operations that fail must propagate as failure — never return `Success: true` with a zero/default value because a follow-up step errored

### Atomic Ordering for Progress Counters

- Use `Relaxed` for `fetch_add`/`load` on shared progress counters (`completed`, `succeeded`, `failed`).
- Use `Release` on `store` and `Acquire` on `load` for the stop-flag (`stop_spinner`).
- Avoid `SeqCst` — emits a full memory fence on every counter update, unnecessary contention.

### Concurrency / Progress Counters

- Increment shared progress counters **after** flushing output, not before.
- All threads that write to the same output stream must share the same mutex.
- `flush()` must happen **outside** the lock.

### Separate Format from Filter

- Functions that format output should not also decide whether to show it.
- Pattern: `fn format_line(status) -> String` + caller `if !status.success || files_changed > 0 || verbose { print }`.

### Comments

- Minimize — write self-documenting code
- Only for non-obvious logic or business decisions

### Git Commits

- Concise, descriptive messages
- No emojis in commit messages or comments
- ALWAYS land the plane: commit, pull --rebase, push. Work is NOT done until `git push` succeeds.

### Functions

- < 20 lines when possible

## Known Issues

### Flaky Tests

`cargo test` is non-deterministic due to parallel test isolation. `test_is_dirty_clean` and `test_update_repository_no_remote` occasionally fail. Re-run before concluding a test failure is caused by a code change.
