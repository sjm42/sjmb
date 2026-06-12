# sjmb

A reconnecting IRC bot written in Rust.

## Features

- **Auto-op/voice** — automatically grants channel operator and voice privileges based on regex ACL patterns
- **Private-message commands** — configurable PM commands for invite, op, voice, join, nick, ACL dump, reload, and say
- **URL title fetching** — detects URLs in channel messages and displays webpage titles
- **Duplicate URL detection** — logs URLs to PostgreSQL and flags duplicates within a configurable time window
- **URL commands** — template-based commands (using Tera) for fetching data from URLs (e.g., METAR/TAF weather reports)
- **URL mutation** — rewrites URLs via regex rules (e.g., Twitter → Nitter)
- **Hot-reloadable config** — reload bot configuration without restarting
- **Channel-specific behavior** — feature flags and duplicate-url settings support wildcard defaults with per-channel overrides
- **Throttled IRC queues** — rate-limits outgoing mode changes and messages, including duplicate `+o` suppression from tracked channel state

## Configuration

The bot uses two configuration files:

- `irc.toml` — IRC connection: server, nick, channels, encoding, and IRC client throttling options
- `sjmb.json` — bot settings: ACLs, PM commands, URL commands, channel feature flags, duplicate URL settings, URL mutations

Default runtime paths are:

- `--bot-config`: `$HOME/sjmb/config/sjmb.json`
- `--irc-config`: `$HOME/sjmb/config/irc.toml`

See [`config/sjmb.json`](./config/sjmb.json) and [`config/irc.toml`](./config/irc.toml) for examples.

Channel feature maps in `sjmb.json` support a `*` fallback entry plus per-channel overrides. URL duplicate reporting also
uses per-channel expiry and timezone maps, with `UTC` as the example default.

## Running

Show CLI options:

```bash
cargo run --bin sjmb -- --help
```

Run with the default config paths:

```bash
cargo run --bin sjmb -- --verbose
```

The bot will reconnect automatically after failures, reloading the process state on each start attempt. Runtime config
can be reloaded with the configured private-message reload command.

Logging defaults to errors only. Use `--verbose`, `--debug`, or `--trace` to increase log detail.

## Building

Requires stable Rust (edition 2024). PostgreSQL is required if URL logging or duplicate URL checks are enabled in the
bot config.

```bash
cargo check
cargo clippy --all-targets --all-features
cargo test
cargo build --release
```

Run `cargo fmt` before committing. To check whether direct dependency updates are available, use:

```bash
cargo outdated --root-deps-only
```

Additional build options:

```bash
cargo build --profile minsize
./build-mips
```

## License

MIT OR Apache-2.0
