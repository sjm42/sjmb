// config.rs

use crate::*;

pub fn load_irc_config(path: &str) -> anyhow::Result<Config> {
    let mut config = Config::load(path)?;
    // Enabling the library's TLS feature must not silently change existing connections.
    config.use_tls.get_or_insert(false);
    if config.ping_time() == 0 {
        bail!("IRC ping_time must be positive");
    }
    Ok(config)
}

#[derive(Debug, Clone, Parser)]
pub struct OptsCommon {
    #[arg(short, long)]
    pub verbose: bool,
    #[arg(short, long)]
    pub debug: bool,
    #[arg(short, long)]
    pub trace: bool,

    #[arg(short, long, default_value = "$HOME/sjmb/config/sjmb.json")]
    pub bot_config: String,
    #[arg(short, long, default_value = "$HOME/sjmb/config/irc.toml")]
    pub irc_config: String,
}

impl OptsCommon {
    pub fn finalize(&mut self) -> anyhow::Result<()> {
        self.bot_config = shellexpand::full(&self.bot_config)?.into_owned();
        self.irc_config = shellexpand::full(&self.irc_config)?.into_owned();
        Ok(())
    }

    pub fn get_loglevel(&self) -> Level {
        if self.trace {
            Level::TRACE
        } else if self.debug {
            Level::DEBUG
        } else if self.verbose {
            Level::INFO
        } else {
            Level::ERROR
        }
    }

    fn log_filter(&self) -> Targets {
        let level = self.get_loglevel();
        Targets::new()
            .with_default(level.min(Level::INFO))
            .with_target(env!("CARGO_CRATE_NAME"), level)
    }

    pub fn start_pgm(&self, name: &str) -> anyhow::Result<()> {
        tracing_fmt()
            .with_max_level(self.get_loglevel())
            .with_target(false)
            .finish()
            .with(self.log_filter())
            .try_init()
            .map_err(|e| anyhow!("Could not initialize tracing: {e}"))?;

        info!("Starting up {name} v{}...", env!("CARGO_PKG_VERSION"));
        debug!("Git branch: {}", env!("GIT_BRANCH"));
        debug!("Git commit: {}", env!("GIT_COMMIT"));
        debug!("Source timestamp: {}", env!("SOURCE_TIMESTAMP"));
        debug!("Compiler version: {}", env!("RUSTC_VERSION"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_verbosity_preserves_dependency_info_but_limits_debug_and_trace_to_our_code() {
        struct TempLog(PathBuf);
        impl Drop for TempLog {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let log = TempLog(env::temp_dir().join(format!("sjmb-log-{}.txt", rand::random::<u64>())));
        for (args, app_info, app_debug, app_trace) in [
            (vec!["sjmb"], false, false, false),
            (vec!["sjmb", "-v"], true, false, false),
            (vec!["sjmb", "--debug"], true, true, false),
            (vec!["sjmb", "--trace"], true, true, true),
        ] {
            let opts = OptsCommon::parse_from(args);
            let subscriber = tracing_fmt()
                .with_max_level(opts.get_loglevel())
                .with_target(false)
                .with_ansi(false)
                .without_time()
                .with_writer(File::create(&log.0).unwrap())
                .finish()
                .with(opts.log_filter());
            with_default(subscriber, || {
                error!(target: "sjmb", "app-error");
                warn!(target: "sjmb::ircbot", "app-warn");
                info!(target: "sjmb", "app-info");
                debug!(target: "sjmb::http_fetch", "app-debug");
                trace!(target: "sjmb::ircbot", "app-trace");
                error!(target: "reqwest", "dependency-error");
                warn!(target: "sqlx", "dependency-warn");
                info!(target: "irc::client", "dependency-info");
                debug!(target: "reqwest::connect", "dependency-debug");
                trace!(target: "hyper::proto", "dependency-trace");
            });
            let text = fs::read_to_string(&log.0).unwrap();
            assert!(text.contains("app-error"));
            assert!(text.contains("dependency-error"));
            assert_eq!(text.contains("app-warn"), app_info);
            assert_eq!(text.contains("app-info"), app_info);
            assert_eq!(text.contains("app-debug"), app_debug);
            assert_eq!(text.contains("app-trace"), app_trace);
            assert_eq!(text.contains("dependency-warn"), app_info);
            assert_eq!(text.contains("dependency-info"), app_info);
            assert!(!text.contains("dependency-debug"));
            assert!(!text.contains("dependency-trace"));
        }
    }

    #[test]
    fn tls_is_opt_in_and_zero_ping_interval_is_rejected() {
        struct TempConfig(PathBuf);
        impl Drop for TempConfig {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let file = TempConfig(env::temp_dir().join(format!("sjmb-irc-{}.toml", rand::random::<u64>())));
        let path = file.0.to_str().unwrap();
        for (settings, tls, port) in [
            ("", false, 6667),
            ("use_tls = false", false, 6667),
            ("use_tls = true", true, 6697),
        ] {
            fs::write(path, format!("nickname = 'bot'\nserver = 'irc.example'\n{settings}\n")).unwrap();
            let config = load_irc_config(path).unwrap();
            assert_eq!(config.use_tls(), tls);
            assert_eq!(config.port(), port);
        }
        fs::write(path, "nickname = 'bot'\nserver = 'irc.example'\nping_time = 0\n").unwrap();
        assert!(load_irc_config(path).is_err());
    }
}
// EOF
