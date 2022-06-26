// config.rs

use chrono::*;
use log::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, fs::File, io::BufReader};
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
    pub url_fetch_channels: HashMap<String, bool>,
    pub url_regex: String,
    pub cmd_invite: String, // magic word to get /invite
    pub cmd_mode_o: String, // magic word to get +o
    pub cmd_mode_v: String, // magic word to get +v
    pub mode_o_acl: String, // json file for +o ACL
    pub auto_o_acl: String, // json file for auto-op ACL
}
impl ConfigCommon {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let file = &opts.bot_config;
        info!("Reading config file {file}");
        let mut config: ConfigCommon = serde_json::from_reader(BufReader::new(File::open(file)?))?;
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        config.mode_o_acl = shellexpand::full(&config.mode_o_acl)?.into_owned();
        config.auto_o_acl = shellexpand::full(&config.auto_o_acl)?.into_owned();
        debug!("New ConfigCommon:\n{config:#?}");
        Ok(config)
    }
}

pub struct BotRuntimeConfig {
    pub common: ConfigCommon,
    pub mode_o_acl: ReAcl,
    pub auto_o_acl: ReAcl,
}
impl BotRuntimeConfig {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let now1 = Utc::now();
        // read & parse json main config
        let common = ConfigCommon::new(opts)?;

        // pre-compile the ACL regex arrays
        debug!("Reading regex array ACLs...");

        // read & parse ACLs in json format
        let mode_o_acl = ReAcl::new(&common.mode_o_acl)?;
        let auto_o_acl = ReAcl::new(&common.auto_o_acl)?;

        debug!(
            "New runtime config successfully created in {} ms.",
            Utc::now().signed_duration_since(now1).num_milliseconds()
        );
        Ok(Self {
            common,
            mode_o_acl,
            auto_o_acl,
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JAcl {
    pub acl: Vec<String>,
}
impl JAcl {
    pub fn new(file: &str) -> anyhow::Result<Self> {
        info!("Reading json acl file {file}");
        let acl: Self = serde_json::from_reader(BufReader::new(File::open(file)?))?;
        info!("Got {} entries.", acl.acl.len());
        debug!("New JAcl:\n{acl:#?}");
        Ok(acl)
    }
}

pub struct ReAcl {
    pub acl_str: Vec<String>,
    pub acl_re: Vec<Regex>,
}
impl ReAcl {
    pub fn new(file: &str) -> anyhow::Result<Self> {
        let jacl = JAcl::new(file)?;
        let mut re_vec = Vec::with_capacity(jacl.acl.len());
        for s in &jacl.acl {
            re_vec.push(Regex::new(s)?);
        }
        Ok(Self {
            acl_str: jacl.acl,
            acl_re: re_vec,
        })
    }
    pub fn re_match<S>(&self, userhost: S) -> Option<(usize, String)>
    where
        S: AsRef<str>,
    {
        for (i, re) in self.acl_re.iter().enumerate() {
            if re.is_match(userhost.as_ref()) {
                // return index of match along with the matched regex string
                return Some((i, self.acl_str[i].to_string()));
            }
        }
        None
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
