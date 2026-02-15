# sjmb

A feature-rich IRC bot written in Rust.

## Features

- **Auto-op/voice** — automatically grants channel operator and voice privileges based on regex ACL patterns
- **Op/voice** per request via configurable private messages
- **URL title fetching** — detects URLs in channel messages and displays webpage titles
- **Duplicate URL detection** — logs URLs to PostgreSQL and flags duplicates within a configurable time window
- **URL commands** — template-based commands (using Tera) for fetching data from URLs (e.g., METAR/TAF weather reports)
- **URL mutation** — rewrites URLs via regex rules (e.g., Twitter → Nitter)
- **Hot-reloadable config** — reload bot configuration without restarting

## Configuration

The bot uses two configuration files:

- `irc.toml` — IRC connection: server, nick, channels
- `sjmb.json` — bot settings: ACLs, URL commands, channel features, URL mutations

See the `config/` directory for examples.

## Building

Requires stable Rust toolchain and a PostgreSQL database for URL logging.

```bash
cargo build --release
```

## License

MIT OR Apache-2.0
