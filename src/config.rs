// config.rs

use log::*;
use regex::Regex;
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

pub struct BotRuntimeConfig {
    pub common: ConfigCommon,
    pub o_acl: OAcl,
    pub o_acl_re: Vec<Regex>,
}
impl BotRuntimeConfig {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let common = ConfigCommon::new(opts)?;
        let o_acl = OAcl::new(&common)?;
        debug!("My ACL:\n{:#?}", &o_acl.acl);
        let o_acl_re = OAcl::to_re(&o_acl)?;
        Ok(Self {
            common,
            o_acl,
            o_acl_re,
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConfigCommon {
    pub irc_log_dir: String,
    pub owner: String,
    pub channel: String,
    pub cmd_invite: String, // magic word to get /invite
    pub cmd_mode_o: String, // magic word to get +o
    pub cmd_mode_v: String, // magic word to get +v
    pub mode_o_acl: String, // json file for +o ACL
}
impl ConfigCommon {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let file = &opts.bot_config;
        debug!("Reading config file {file}");
        let mut config: ConfigCommon = serde_json::from_reader(BufReader::new(File::open(file)?))?;
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        config.mode_o_acl = shellexpand::full(&config.mode_o_acl)?.into_owned();
        debug!("New ConfigCommon:\n{config:#?}");
        Ok(config)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OAcl {
    pub acl: Vec<String>,
}
impl OAcl {
    pub fn new(cfg: &ConfigCommon) -> anyhow::Result<Self> {
        let file = &cfg.mode_o_acl;
        debug!("Reading o_acl file {file}");
        let o_acl = serde_json::from_reader(BufReader::new(File::open(file)?))?;
        debug!("New OAcl:\n{o_acl:#?}");
        Ok(o_acl)
    }
    pub fn to_re(&self) -> anyhow::Result<Vec<Regex>> {
        let mut re_vec = Vec::new();
        for s in &self.acl {
            re_vec.push(Regex::new(s)?);
        }
        Ok(re_vec)
    }
    pub fn re_match<S>(acl: &[Regex], userhost: S) -> bool
    where
        S: AsRef<str>,
    {
        acl.iter().any(|re| re.is_match(userhost.as_ref()))
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
