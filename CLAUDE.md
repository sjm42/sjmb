# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**sjmb** is an IRC bot written in Rust. It provides auto-op/voice via regex ACLs, URL title fetching, duplicate URL
detection (PostgreSQL-backed), template-based URL commands (e.g., METAR weather), and URL mutation (e.g., Twitter →
Nitter redirects).

## Build Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build (LTO, opt-level=3)
cargo build --profile minsize  # Minimal size release (stripped, opt-level=z)
cargo clippy                   # Lint
cargo fmt                      # Format (max_width=120, crate-level import grouping)
```

There are no tests in this project. The toolchain is stable Rust, edition 2024.

## Architecture

```
src/
├── lib.rs          - Module exports and common re-imports (std, anyhow, tokio, irc, etc.)
├── bin/sjmb.rs     - Entry point: CLI parsing, reconnection loop, handler registration
├── ircbot.rs       - Core bot: IrcBot struct, config loading, message routing, URL handling
├── config.rs       - OptsCommon (clap CLI args), logging setup, default config paths
├── db_util.rs      - PostgreSQL operations: URL logging, duplicate detection with retry logic
└── util.rs         - Helpers: ReAcl (regex ACLs), ReMut (regex mutations), HTTP client, traits
```

### Key Flow

1. `bin/sjmb.rs` parses CLI args, enters reconnection loop (10s retry)
2. `IrcBot::new()` loads JSON config (`~/sjmb/config/sjmb.json`), compiles regex ACLs/mutations, connects IRC client
   from `irc.toml`
3. `IrcBot::run()` streams IRC messages, routes to handlers
4. Channel messages: detect URLs → check blacklist → log to DB → fetch titles → apply URL mutations
5. Operations/messages queued via `mpsc` channels with throttling (ops: 2.5s, msgs: 1.5s) to avoid IRC rate limits

### Configuration

- **Bot config**: JSON file (default `$HOME/sjmb/config/sjmb.json`) — ACLs, URL commands, channel settings, mutations
- **IRC config**: TOML file (default `$HOME/sjmb/config/irc.toml`) — server, nick, channels
- Config supports hot-reload via `!reload` command
- Channel settings use wildcard maps (`"*"` key as fallback, channel-specific overrides)

### Concurrency Pattern

All shared state lives in `Arc<RwLock<>>`. Background tokio tasks handle message/op queues. Inter-task communication
uses `mpsc::unbounded_channel`.

### Notable Constraints

- **SQLx TLS disabled intentionally** — connections hang with TLS enabled (see Cargo.toml comment)
- **HTTP client disables certificate verification** (`danger_accept_invalid_certs`) for URL fetching
- Regex ACLs and URL patterns are pre-compiled at config load time for performance
