// config.rs

use std::env;

use clap::Parser;

use crate::*;

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

    pub fn start_pgm(&self, name: &str) {
        tracing_subscriber::fmt()
            .with_max_level(self.get_loglevel())
            .with_target(false)
            .init();

        info!("Starting up {name} v{}...", env!("CARGO_PKG_VERSION"));
        debug!("Git branch: {}", env!("GIT_BRANCH"));
        debug!("Git commit: {}", env!("GIT_COMMIT"));
        debug!("Source timestamp: {}", env!("SOURCE_TIMESTAMP"));
        debug!("Compiler version: {}", env!("RUSTC_VERSION"));
    }
}
// EOF
