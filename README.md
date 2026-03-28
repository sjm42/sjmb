# sjmb

A feature-rich IRC bot written in Rust.

## Features

- **Auto-op/voice** — automatically grants channel operator and voice privileges based on regex ACL patterns
- **Private-message commands** — configurable PM commands for invite, op, voice, join, nick, ACL dump, reload, and say
- **URL title fetching** — detects URLs in channel messages and displays webpage titles
- **Duplicate URL detection** — logs URLs to PostgreSQL and flags duplicates within a configurable time window
- **URL commands** — template-based commands (using Tera) for fetching data from URLs (e.g., METAR/TAF weather reports)
- **URL mutation** — rewrites URLs via regex rules (e.g., Twitter → Nitter)
- **Hot-reloadable config** — reload bot configuration without restarting
- **Channel-specific behavior** — feature flags and duplicate-url settings support wildcard defaults with per-channel overrides

## Configuration

The bot uses two configuration files:

- `irc.toml` — IRC connection: server, nick, channels
- `sjmb.json` — bot settings: ACLs, PM commands, URL commands, channel feature flags, duplicate URL settings, URL mutations

Default runtime paths are:

- `--bot-config`: `$HOME/sjmb/config/sjmb.json`
- `--irc-config`: `$HOME/sjmb/config/irc.toml`

See [`config/sjmb.json`](./config/sjmb.json) and [`config/irc.toml`](./config/irc.toml) for examples.

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

## Building

Requires stable Rust (edition 2024). PostgreSQL is required if URL logging or duplicate URL checks are enabled in the
bot config.

```bash
cargo check
cargo clippy --all-targets --all-features
cargo test
cargo build --release
```

Additional build options:

```bash
cargo build --profile minsize
./build-mips
```

## License

MIT OR Apache-2.0
