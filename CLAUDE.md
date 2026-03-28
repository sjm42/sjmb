# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**sjmb** is an IRC bot written in Rust. It provides auto-op/voice via regex ACLs, URL title fetching, duplicate URL
detection (PostgreSQL-backed), template-based URL commands (e.g., METAR weather), URL mutation (e.g., Twitter →
Nitter redirects), private-message bot commands, and hot-reloadable runtime config.

## Build Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build (LTO, opt-level=3)
cargo build --profile minsize  # Minimal size release (stripped, opt-level=z)
cargo check                    # Fast compile check
cargo clippy --all-targets --all-features
cargo fmt                      # Format (max_width=120, crate-level import grouping)
cargo test                     # Test/compile smoke pass
```

There are currently no committed unit or integration tests in this project, so `cargo test` is mainly a build/target
verification step. The toolchain is stable Rust, edition 2024.

## Architecture

```
src/
├── lib.rs          - Module exports and common re-imports (std, anyhow, tokio, irc, etc.)
├── bin/sjmb.rs     - Entry point: CLI parsing, reconnection loop, handler registration
├── ircbot.rs       - Core bot: IrcBot struct, config loading/reload, message routing, URL handling, queued IRC ops
├── config.rs       - OptsCommon (clap CLI args), logging setup, default config paths
├── db_util.rs      - PostgreSQL operations: URL logging, duplicate detection with retry logic
└── util.rs         - Helpers: ReAcl (regex ACLs), ReMut (regex mutations), wildcard lookups, HTTP client, traits
```

### Key Flow

1. `bin/sjmb.rs` parses CLI args, expands config paths, initializes tracing, then enters a 10 second reconnect loop
2. `IrcBot::new()` loads JSON config (`~/sjmb/config/sjmb.json`), compiles regex ACLs/mutations, connects IRC client
   from `irc.toml`
3. `IrcBot::run()` streams IRC messages, routes to handlers
4. Private messages dispatch either privileged commands (`dumpacl`, `join`, `nick`, `reload`, `say`) or open commands
   (`invite`, `mode_o`, `mode_v`)
5. Channel messages: detect `!` URL commands or inline URLs → check blacklist → log/check duplicates → fetch titles →
   apply URL mutations
6. Operations/messages are queued via `mpsc` channels with throttling (ops: 2.5s, msgs: 1.5s) to avoid IRC rate limits

### Configuration

- **Bot config**: JSON file (default `$HOME/sjmb/config/sjmb.json`) — ACLs, URL commands, channel settings, mutations
- **IRC config**: TOML file (default `$HOME/sjmb/config/irc.toml`) — server, nick, channels
- Config supports hot-reload via `!reload` command
- Channel feature flags and duplicate-url settings use wildcard maps (`"*"` key as fallback, channel-specific overrides)
- URL command templates are rendered with Tera and receive both `arg` and split `args`

### Concurrency Pattern

All shared state lives in `Arc<RwLock<>>`. Background tokio tasks handle message/op queues. Inter-task communication
uses `mpsc::unbounded_channel`.

### Notable Constraints

- **SQLx TLS disabled intentionally** — connections hang with TLS enabled (see Cargo.toml comment)
- **HTTP client disables certificate verification** (`danger_accept_invalid_certs`) for URL fetching
- Regex ACLs and URL patterns are pre-compiled at config load time for performance
