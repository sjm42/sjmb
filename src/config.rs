// config.rs

use log::*;
use std::env;
use structopt::StructOpt;

#[derive(Debug, Clone, StructOpt)]
pub struct OptsCommon {
    #[structopt(short, long)]
    pub verbose: bool,
    #[structopt(short, long)]
    pub debug: bool,
    #[structopt(short, long)]
    pub trace: bool,

    #[structopt(short, long, default_value = "$HOME/sjmb/config/sjmb.json")]
    pub bot_config: String,
    #[structopt(short, long, default_value = "$HOME/sjmb/config/irc.toml")]
    pub irc_config: String,
}

impl OptsCommon {
    pub fn finish(&mut self) -> anyhow::Result<()> {
        self.bot_config = shellexpand::full(&self.bot_config)?.into_owned();
        self.irc_config = shellexpand::full(&self.irc_config)?.into_owned();
        Ok(())
    }
    pub fn get_loglevel(&self) -> LevelFilter {
        if self.trace {
            LevelFilter::Trace
        } else if self.debug {
            LevelFilter::Debug
        } else if self.verbose {
            LevelFilter::Info
        } else {
            LevelFilter::Error
        }
    }
    pub fn start_pgm(&self, name: &str) {
        env_logger::Builder::new()
            .filter_module(env!("CARGO_PKG_NAME"), self.get_loglevel())
            .filter_module(name, self.get_loglevel())
            .format_timestamp_secs()
            .init();
        info!(
            "Starting up {} v{}...",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        );
        debug!("Git branch: {}", env!("GIT_BRANCH"));
        debug!("Git commit: {}", env!("GIT_COMMIT"));
        debug!("Source timestamp: {}", env!("SOURCE_TIMESTAMP"));
        debug!("Compiler version: {}", env!("RUSTC_VERSION"));
    }
}

// EOF
