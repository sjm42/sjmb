# sjmb

A reconnecting IRC bot written in Rust.

## Features

- **Auto-op/voice** — automatically grants channel operator and voice privileges based on regex ACL patterns
- **Private-message commands** — configurable PM commands for invite, op, voice, join, nick, ACL dump, reload, and say
- **URL title fetching** — detects URLs in channel messages and displays webpage titles
- **Duplicate URL detection** — logs URLs to PostgreSQL and flags duplicates within a configurable time window
- **URL commands** — template-based commands (using Tera 2) for fetching data from URLs (e.g., METAR/TAF weather reports)
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
Nickname and channel lookups follow the server's `CASEMAPPING` (`ascii`, `strict-rfc1459`, or `rfc1459`, the default).
Case-equivalent keys in the same configuration map are rejected when looked up. A change of case mapping after channel
tracking begins causes a reconnect to rebuild state safely.

IRC TLS support is included using Rustls. To enable it, set `use_tls = true` and the server's TLS port (usually `6697`)
in `irc.toml`. Omitting `use_tls` preserves plaintext operation for existing configurations. Normal certificate and
hostname verification remain enabled. HTTPS fetching already uses Rustls; SQLx's TLS settings are unchanged.

URL command templates receive `arg` (the complete command argument string) and `args` (the whitespace-separated argument
list). The Tera 1 `slugify` filter remains available as a compatibility filter after the Tera 2 migration.

Administrative private messages can additionally require a matching `user@host` mask:

```json
"privileged_userhosts": {
  "sjm": ["~sjm@*.example.net", "sjm@my-bouncer.example.org"]
}
```

The nickname must also be enabled in `privileged_nicks`. Masks match the whole, case-sensitive `user@host` string;
`*` matches any sequence and `?` matches one character. Other characters are literal. An empty mask list denies
administrative commands for that nickname. Omitting a nickname from this optional map preserves its existing nick-only
authorization. Replace the example mask in `config/sjmb.json` with your own userhost before using it.
These masks use simple wildcard syntax, with escaped regular expressions used internally. The separate existing ACL
lists, such as `auto_o_acl` and `mode_o_acl`, continue to use regular expressions.

Bot configuration is limited to 1 MiB. Command names must be unique, nonempty single words, and extraction regexes
(`url_regex` and each `output_filter`) must include capture group 1. Optional captures that did not match are skipped.
URL expiry days must be nonnegative and fit in the timestamp calculations. Missing `cmd_version` defaults to `version`.
Reload prepares and validates the replacement before publishing it and updating command registrations; a failed reload
leaves the working settings intact.

## Resource and network limits

One serial application worker handles incoming callbacks and queued operations. The socket loop continues processing IRC
while that worker is waiting for HTTP or PostgreSQL. Input, operation, and output queues each hold 42 items. When a queue
is full, new work is discarded. Queued requests, operations, and replies expire three minutes after their original
request; moving between queues does not reset that deadline. Work already started may finish, but late replies are
discarded. Aggregate overflow and expiry counts are logged every 30 seconds at error level. There are no
per-user or per-channel request budgets. Outgoing text is normalized to one line and limited to 400 UTF-8 bytes (or less
when the target requires more space). Each channel message examines at most four URL matches; each URL command produces
at most four matching replies.

All application output uses the throttled output queue. After each batch of 16 commands, a PING checkpoint must receive
its matching PONG within 20 seconds before more application output is passed to the IRC library. This bounds our
contribution to the library's internal queue without modifying that dependency. The library still owns its protocol
traffic, automatic CTCP replies, and incoming framing; its internal buffers are not replaced with bounded buffers.
Automatic CTCP replies remain enabled as an accepted tradeoff: they bypass the application's limits, and the library
also replies to channel-directed queries in that channel. Occasional queries are harmless; sustained queries can
create an outgoing backlog or channel flooding.
Pending operator grants are discarded if channel membership changed since the triggering message arrived, and channel
operator state is updated from server messages rather than presumed from successful queue submission.
The bot maintains this state itself; the IRC library's redundant `channel-lists` feature is disabled.

HTTP fetching uses a shared client, a 5-second connection timeout, and a 10-second overall timeout covering DNS, redirects,
and response reading. URLs are limited to 4 KiB with at most five redirects. Non-text bodies are skipped before reading.
The first 1 MiB of decompressed response bytes is retained, decoded according to the response charset, and bounded to
1 MiB of UTF-8 text. Reading stops at the limit: a large page can still yield a title or complete URL-command records.
A capture ending exactly at a truncated body boundary is skipped because it may be incomplete. Title extraction uses
HTML tokenization without building a DOM or walking nested elements recursively.

The same destination policy applies to titles, command templates, rewritten URLs, and every redirect. HTTP/HTTPS URLs
with embedded credentials are rejected. Literal addresses and the addresses returned by the connection's DNS resolver
must pass the public-address policy; a hostname with any rejected answer is refused. Environment HTTP proxies are
disabled so connections cannot bypass this policy. Nonstandard public HTTP ports remain supported.

The conservative address policy excludes IPv4 private ranges, `0.0.0.0/8`, `100.64.0.0/10`, `127.0.0.0/8`, link-local
`169.254.0.0/16`, special protocol assignments `192.0.0.0/24`, deprecated `192.88.99.0/24`, benchmarking
`198.18.0.0/15`, documentation prefixes, multicast, and reserved space. IPv6 is restricted to native global unicast
`2000::/3`, excluding special assignments `2001::/23`, documentation `2001:db8::/32` and `3fff::/20`, and 6to4
`2002::/16`. This also excludes IPv6 loopback, unspecified, ULA, link-local, multicast, NAT64 and IPv4-mapped addresses,
including mapped public IPv4 addresses. These intentionally conservative exclusions follow the
[IANA IPv4](https://www.iana.org/assignments/iana-ipv4-special-registry/) and
[IPv6](https://www.iana.org/assignments/iana-ipv6-special-registry/) special-purpose registries.

PostgreSQL connects lazily, so an outage does not prevent IRC startup. Pool acquisition is limited to 3 seconds;
connections use a 5-second statement timeout and a 1-second lock timeout. Each database job has an 8-second overall
deadline. Failed inserts/checks are logged and processing continues, without application-level retries. No retention,
schema, index, or deduplication changes are made.

## Running

Show CLI options:

```bash
cargo run --bin sjmb -- --help
```

Run with the default config paths:

```bash
cargo run --bin sjmb -- --verbose
```

The bot reconnects after connection, registration, or session failures, reloading its state on each attempt. Connection
and registration each have a 30-second deadline. Retry delays grow from 2 to 30 seconds with up to one second of jitter,
resetting after a session lasts at least a minute. The serial worker is scoped to its session and is cancelled when
that session ends. Invalid initial configuration fails clearly. Runtime config can be reloaded with the configured
private-message reload command.

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
