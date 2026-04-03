# Documentation Style Guide

Standards for docs in the groppy project. Primary audience: AI coding agents. Secondary audience: humans.

## Philosophy

- **Agents first**: Docs are consumed as context by LLMs. Optimize for token efficiency and parseability.
- **Density over prose**: Pack maximum information into minimum tokens. No filler, no transitions, no "welcome to" preamble.
- **Flat over nested**: Prefer one doc with sections over many small files that each cost a tool call to read.
- **Structured over narrative**: Tables, key-value pairs, and code blocks parse faster than paragraphs.
- **Commands are copy-paste**: Every command block must run verbatim. No placeholders unless labeled `<placeholder>`.
- **Single source of truth**: Never duplicate information across files. Reference instead.

## Formatting Rules

### Structure

- H1 = document title, one per file
- H2 = major sections
- No H1 → H3 skips
- No blank section bodies — delete the header if there's nothing under it

### Data

- Tables for structured data (flags, commands, comparisons) — flag tables: short and long forms in same row
- Inline code for any value an agent might grep or copy: paths, flags, commands, identifiers
- Code blocks with language tag for anything > one line
- Language tags: `bash`, `rust`, `toml`, `text`

### Text

- No articles ("the", "a") where meaning is preserved without them
- No hedging ("might", "could", "you may want to")
- Imperative mood: "Run X" not "You can run X"
- One fact per bullet. No multi-sentence bullets.

### Anti-patterns

- Repeating information already in another doc (reference it)
- "Welcome to" / "This document describes" / "In this section" preamble
- Narrative paragraphs where a table or list suffices
- Inline links with long display text — use `[short label](path)` or bare relative path
- Blank filler sections ("More coming soon")

## Maintenance

Update docs in the SAME commit as code changes. If a command in docs doesn't run, the docs are broken — fix immediately.

- CLI flag added or changed → update flag table in `usage.md` in the same commit
- Flag tables: list short and long forms together in one row (`-v`, `--verbose`)

## TODO Workflow

All TODOs must be recorded in `docs/TODO.md` before being actioned, to survive session interruption. Write to `docs/TODO.md` first — before using any UI todo tool.

- Add item to **Pending** before starting work
- Move to **Completed** table with date (`YYYY-MM-DD`) when done
- Remove completed items after 14 days — not before

## Session Completion

Every session MUST end with the plane landed. Work is not done until `git push` succeeds.

```bash
git pull --rebase
git push
git status  # must show "up to date with origin"
```

- Never stop before pushing — leaves work stranded locally
- Update `docs/TODO.md` completed items before the final commit
