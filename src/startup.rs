// startup.rs

use log::*;
use serde::{Deserialize, Serialize};
use std::{env, fs::File, io::BufReader};
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConfigCommon {
    pub irc_log_dir: String,
    pub owner: String,
    pub channel: String,
    pub o_password: String,
    pub v_password: String,
}
impl ConfigCommon {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        debug!("Reading config file {}", &opts.bot_config);
        let mut config: ConfigCommon =
            serde_json::from_reader(BufReader::new(File::open(&opts.bot_config)?))?;
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        Ok(config)
    }
}

pub fn start_pgm(c: &OptsCommon, name: &str) {
    env_logger::Builder::new()
        .filter_module(name, c.get_loglevel())
        .format_timestamp_secs()
        .init();
    info!("Starting up {name}...");
    debug!("Git branch: {}", env!("GIT_BRANCH"));
    debug!("Git commit: {}", env!("GIT_COMMIT"));
    debug!("Source timestamp: {}", env!("SOURCE_TIMESTAMP"));
    debug!("Compiler version: {}", env!("RUSTC_VERSION"));
}
// EOF
