# AGENTS.md

## Project

**mapscii-rust** - a Braille & ASCII world map renderer for the terminal, written in
Rust. A feature-complete rewrite of
[rastapasta/mapscii](https://github.com/rastapasta/mapscii).

Renders Mapbox vector tiles as Unicode Braille characters with xterm-256 colors
directly in the terminal. Ships as both a **standalone CLI application** (`mapscii`)
and an **embeddable ratatui `StatefulWidget`** library (`mapscii-core`). MIT license.
Documentation language: English.

## Commands

Cargo workspace at the root - run cargo directly, there is no Makefile.

```sh
cargo build --release                        # build both crates
cargo run                                    # run the CLI
cargo install --path mapscii                 # install the CLI from source
cargo test                                   # workspace test suite
cargo fmt --all -- --check                   # formatting gate
cargo clippy --all-targets -- -D warnings    # lint gate
```

Run fmt, clippy and test before declaring work done (CI adds `--all-features` to
clippy).

## Structure

- `mapscii/` - binary crate (`src/main.rs`): the CLI entrypoint.
- `mapscii-core/` - library crate. Modules: `lib.rs`, `config.rs` (MapConfig),
  `utils.rs`, `braille_buffer.rs`, `canvas.rs`, `label_buffer.rs`, `styler.rs`,
  `tile.rs`, `tile_source.rs`, `renderer.rs`, `overlay.rs`, `widget.rs`.
- `styles/dark.json` - bundled Mapbox GL dark style.
- `Cargo.toml` / `Cargo.lock` - workspace root defining the two member crates.
- `.devtools/` - git submodule (`ostara-labs/devtools`): shared hooks and reusable CI
  workflows. Bump the pointer deliberately, in a dedicated commit.
- `.github/` - `CODEOWNERS` and `workflows/` (thin callers only).

## CI contract

`.github/workflows/pr-pipeline.yml` is a thin caller pinned to an immutable
`ostara-labs/devtools` digest (v1.12.0), chaining `ci` -> `ai-review` -> `merge-gate`.

- `ci / rust / rust` - `cargo fmt --all -- --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test` (compiles the workspace).
- `ci / core`, `ci / docs-drift / Docs drift (DOC_MAP)`, `ci / elixir / elixir`,
  `ci / typescript / typescript`, `ci / python / python`, `ci / gate` - org aggregate
  from the same digest; stacks absent from this repo succeed vacuously.
- `merge-gate` - required check, created once CI and the review are terminal on the
  head SHA.
- Blocking labels: `possible security issue`, `size/too-big`. Removing a blocking
  label is the audited override - a human action, never automation.

Draft PRs skip `ai-review` (zero model spend); the review runs only on non-draft PRs
whose CI is green.

## AI review

Every non-draft PR gets an AI review (PR-Agent + OpenRouter). This file is loaded as
the repository context on the default branch, so it is both agent guidance and
reviewer calibration. Keep it accurate.

- Process every finding: fix the code, or reply with a written justification and
  resolve the thread when the finding is wrong. Silence is not resolution. A push
  re-runs the review - iterate until a full round produces zero findings.
- AI review informs; it never replaces human approval on CODEOWNERS trust-boundary
  paths.

## Boundaries

### Always

- Run fmt + clippy + test before declaring work done.
- Add tests for behavior changes.
- Update `README.md` when the CLI surface or the public API changes.
- Use conventional commits (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:` ...).

### Ask first

- Add or upgrade dependencies.
- Edit `.github/workflows/**` or `.github/CODEOWNERS`.
- Bump the `.devtools` submodule pointer.

### Never

- Commit directly to `main`.
- Force-push `main` or a shared branch.
- Bypass or weaken hooks or CI gates (`--no-verify`, skipped checks, relaxed lint).
- Commit secrets or `.env` files.
- Approve or merge your own changes on trust-boundary paths - code ownership is human.

## PR flow

1. Branch from `origin/main`; push and open a **draft** PR (`gh pr create --draft`).
2. Drafts run CI only - push freely while iterating.
3. When the content is complete and `ci / gate` is green, promote it: `gh pr ready`.
   Promotion is what triggers `ai-review` and `merge-gate`.
4. Process every `ai-review` finding, push, repeat until zero findings and zero open
   threads; merge only when required checks are green and no blocking label is present.