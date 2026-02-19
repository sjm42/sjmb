# Repository Guidelines

## Project Structure & Module Organization
Core code lives in `src/`:
- `src/bin/sjmb.rs`: binary entrypoint, CLI parsing, reconnect loop, command registration.
- `src/ircbot.rs`: main bot runtime, message routing, URL handling, ACL checks.
- `src/config.rs`: shared CLI options and logging setup.
- `src/db_util.rs`: PostgreSQL URL logging and duplicate-check queries.
- `src/util.rs`: reusable helpers (regex ACL/mutation wrappers, HTTP fetch helpers, timestamp formatting).
- `src/lib.rs`: module exports and shared re-exports.

Configuration examples are in `config/` (`irc.toml`, `sjmb.json`). Build metadata is injected by `build.rs`.

## Build, Test, and Development Commands
- `cargo build`: debug build for local development.
- `cargo build --release`: optimized release build.
- `cargo build --profile minsize`: size-optimized build profile.
- `cargo run --bin sjmb -- --help`: view CLI flags (`--bot-config`, `--irc-config`, log level flags).
- `cargo check`: fast compile checks before committing.
- `cargo clippy --all-targets --all-features`: lint pass.
- `cargo fmt`: format code (`rustfmt.toml` enforces `max_width = 120`).

Optional cross-build: `./build-mips` (uses `cross` for `mipsel-unknown-linux-musl`).

## Coding Style & Naming Conventions
Use standard Rust style (4-space indentation, snake_case for functions/variables, PascalCase for types, SCREAMING_SNAKE_CASE for constants). Prefer explicit error propagation with `anyhow::Result` and `?`. Keep modules focused; place IRC-domain logic in `ircbot.rs` and DB access in `db_util.rs`.

Run `cargo fmt` before opening a PR. Use `cargo clippy` to catch correctness/style issues early.

## Testing Guidelines
There is currently no `tests/` directory and no committed unit tests. For new logic, add focused unit tests near the module (`mod tests`) or integration tests under `tests/` when behavior crosses modules. Validate at minimum with:
- `cargo check`
- `cargo clippy --all-targets --all-features`
- `cargo test`

## Commit & Pull Request Guidelines
Recent history favors short, imperative commit subjects (for example, `cargo update`, `Refactoring`). Follow that pattern:
- Keep subject lines concise and action-oriented.
- Group related changes in one commit.
- Explain non-obvious config/runtime impacts in the body.

PRs should include:
- What changed and why.
- Any config/schema/runtime implications (especially PostgreSQL or IRC behavior).
- Exact verification commands you ran.
