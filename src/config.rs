// config.rs

use chrono::*;
use log::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, fs::File, io::BufReader};
use structopt::StructOpt;
use tera::Tera;

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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UrlCmd {
    pub url_tmpl: String, // a Tera template string with {{arg}} if command needs an argument
    pub output_filter: String,
    #[serde(skip)]
    pub output_filter_re: Option<Regex>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BotConfig {
    pub irc_log_dir: String,
    pub channel: String,
    pub privileged_nicks: HashMap<String, bool>,

    pub url_fetch_channels: HashMap<String, bool>,
    pub url_regex: String,

    pub cmd_invite: String, // magic word to get /invite
    pub cmd_mode_o: String, // magic word to get +o
    pub cmd_mode_v: String, // magic word to get +v
    pub mode_o_acl: String, // json file for +o ACL
    pub auto_o_acl: String, // json file for auto-op ACL

    pub url_cmd_list: HashMap<String, UrlCmd>,

    #[serde(skip)]
    pub mode_o_acl_rt: Option<ReAcl>,
    #[serde(skip)]
    pub auto_o_acl_rt: Option<ReAcl>,

    #[serde(skip)]
    pub url_regex_re: Option<Regex>,
    #[serde(skip)]
    pub url_cmd_tera: Option<Tera>,
}

impl BotConfig {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let now1 = Utc::now();

        let file = &opts.bot_config;
        info!("Reading config file {file}");
        let mut config: BotConfig = serde_json::from_reader(BufReader::new(File::open(file)?))?;

        // Expand $HOME where relevant
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        config.mode_o_acl = shellexpand::full(&config.mode_o_acl)?.into_owned();
        config.auto_o_acl = shellexpand::full(&config.auto_o_acl)?.into_owned();

        // read in & parse ACLs (json)
        config.mode_o_acl_rt = Some(ReAcl::new(&config.mode_o_acl)?);
        config.auto_o_acl_rt = Some(ReAcl::new(&config.auto_o_acl)?);

        // pre-compile url detection regex
        config.url_regex_re = Some(Regex::new(&config.url_regex)?);

        // prepare url-based commands, if any
        let mut tera = Tera::default();
        for (k, c) in config.url_cmd_list.iter_mut() {
            tera.add_raw_template(k, &c.url_tmpl)?;
            c.output_filter_re = Some(Regex::new(&c.output_filter)?);
        }
        config.url_cmd_tera = Some(tera);

        info!(
            "New runtime config successfully created in {} ms.",
            Utc::now().signed_duration_since(now1).num_milliseconds()
        );
        debug!("New BotConfig:\n{config:#?}");

        Ok(config)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReAcl {
    pub acl: Vec<String>,
    #[serde(skip)]
    pub acl_re: Option<Vec<Regex>>,
}
impl ReAcl {
    pub fn new(file: &str) -> anyhow::Result<Self> {
        info!("Reading json acl file {file}");
        let mut acl: Self = serde_json::from_reader(BufReader::new(File::open(file)?))?;
        info!("Got {} entries.", acl.acl.len());
        debug!("New ReAcl:\n{acl:#?}");

        // precompile every regex and save them
        let mut re_vec = Vec::with_capacity(acl.acl.len());
        for s in &acl.acl {
            re_vec.push(Regex::new(s)?);
        }
        acl.acl_re = Some(re_vec);

        Ok(acl)
    }
    pub fn re_match<S>(&self, userhost: S) -> Option<(usize, String)>
    where
        S: AsRef<str>,
    {
        for (i, re) in self.acl_re.as_ref().unwrap().iter().enumerate() {
            if re.is_match(userhost.as_ref()) {
                // return index of match along with the matched regex string
                return Some((i, self.acl[i].to_string()));
            }
        }
        None
    }
}

// EOF
