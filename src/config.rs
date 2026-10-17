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

    pub fn start_pgm(&self, name: &str) -> anyhow::Result<()> {
        tracing_fmt()
            .with_max_level(self.get_loglevel())
            .with_target(false)
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
