// ircbot.rs

use chrono::*;
use futures::prelude::*;
use irc::client::prelude::*;
use log::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::ops::Deref;
use std::{collections::HashMap, fmt::Display, fs::File, io::BufReader, sync::Arc, time};
use tera::Tera;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{sleep, Duration};
use url::Url;
use webpage::{Webpage, WebpageOptions}; // provides `try_next`

use crate::*;

const INITIAL_SIZE: usize = 32;
const IRCMODE_RATE: u64 = 5; // in seconds

pub type IrcCmdHandler = fn(&IrcBot, &irc::proto::Command) -> anyhow::Result<bool>;
pub type MsgHandler = fn(&mut IrcBot, &str, &str, &str) -> anyhow::Result<bool>;

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

#[derive(Debug, Clone)]
enum MyMode {
    Voice,
    Oper,
}

#[derive(Debug, Clone)]
struct ModeOper {
    mode: MyMode,
    channel: String,
    nick: String,
}

pub struct IrcBot {
    pub irc: Client,
    pub irc_sender: Arc<Sender>,
    pub opts: OptsCommon,
    pub bot_cfg: BotConfig,
    mynick: String,
    msg_nick: String,
    msg_user: String,
    msg_host: String,
    msg_userhost: String,

    op_sender: Option<Arc<UnboundedSender<ModeOper>>>,
    handlers_irc_cmd: Vec<IrcCmdHandler>,
    handlers_privmsg_open: HashMap<String, MsgHandler>,
    handlers_privmsg_priv: HashMap<String, MsgHandler>,
    handlers_chanmsg: HashMap<String, MsgHandler>,
}

impl IrcBot {
    pub async fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let bot_cfg = match BotConfig::new(opts) {
            Ok(b) => b,
            Err(e) => {
                anyhow::bail!("{e}");
            }
        };

        let irc = match Client::new(&opts.irc_config).await {
            Ok(c) => c,
            Err(e) => {
                anyhow::bail!("{e}");
            }
        };

        if let Err(e) = irc.identify() {
            anyhow::bail!("{e}");
        }

        let mynick = irc.current_nickname().to_string();
        let sender = irc.sender();
        Ok(IrcBot {
            irc,
            irc_sender: Arc::new(sender),
            opts: opts.clone(),
            bot_cfg,
            mynick,
            msg_nick: "NONE".into(),
            msg_user: "NONE".into(),
            msg_host: "NONE".into(),
            msg_userhost: "NONE@NONE".into(),
            op_sender: None,
            handlers_irc_cmd: Vec::with_capacity(INITIAL_SIZE),
            handlers_privmsg_open: HashMap::with_capacity(INITIAL_SIZE),
            handlers_privmsg_priv: HashMap::with_capacity(INITIAL_SIZE),
            handlers_chanmsg: HashMap::with_capacity(INITIAL_SIZE),
        })
    }

    pub fn clear_handlers(&mut self) {
        self.handlers_irc_cmd.clear();
        self.handlers_privmsg_open.clear();
        self.handlers_privmsg_priv.clear();
        self.handlers_chanmsg.clear();
    }

    pub fn reload(&mut self) -> anyhow::Result<bool> {
        match BotConfig::new(&self.opts) {
            Ok(cfg) => {
                let msg = "*** Reload successful.";
                info!("{msg}");
                self.bot_cfg = cfg;
                Ok(true)
            }
            Err(e) => {
                let msg = "*** Reload failed.";
                error!("{msg}");
                let msg = format!(
                    "Could not parse runtime config {c}: {e}",
                    c = &self.opts.bot_config
                );
                error!("{msg}");
                Err(anyhow::anyhow!(msg))
            }
        }
    }

    pub fn mynick(&self) -> &str {
        &self.mynick
    }
    pub fn msg_nick(&self) -> &str {
        &self.msg_nick
    }
    pub fn msg_user(&self) -> &str {
        &self.msg_user
    }
    pub fn msg_host(&self) -> &str {
        &self.msg_host
    }
    pub fn msg_userhost(&self) -> &str {
        &self.msg_userhost
    }

    pub fn register_irc_cmd(&mut self, handler: IrcCmdHandler) {
        self.handlers_irc_cmd.push(handler);
    }

    pub fn register_privmsg_priv<S>(&mut self, cmd: S, handler: MsgHandler)
    where
        S: AsRef<str> + Display,
    {
        self.handlers_privmsg_priv.insert(cmd.to_string(), handler);
    }

    pub fn register_privmsg_open<S>(&mut self, cmd: S, handler: MsgHandler)
    where
        S: AsRef<str> + Display,
    {
        self.handlers_privmsg_open.insert(cmd.to_string(), handler);
    }

    pub fn register_chanmsg<S>(&mut self, cmd: S, handler: MsgHandler)
    where
        S: AsRef<str> + Display,
    {
        self.handlers_chanmsg.insert(cmd.to_string(), handler);
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        self.start_op_queue();
        let mut stream = self.irc.stream()?;
        while let Some(message) = stream.next().await.transpose()? {
            trace!("Got msg: {message:?}");
            let mynick = self.mynick().to_string();

            let msg_nick;
            let msg_user;
            let msg_host;

            if let Some(Prefix::Nickname(nick, user, host)) = message.prefix {
                (msg_nick, msg_user, msg_host) = (nick, user, host);
            } else {
                (msg_nick, msg_user, msg_host) = ("NONE".into(), "NONE".into(), "NONE".into());
            }
            self.msg_nick = msg_nick.clone();
            self.msg_user = msg_user.clone();
            self.msg_host = msg_host.clone();
            let userhost = format!("{msg_user}@{msg_host}");
            self.msg_userhost = userhost.clone();

            for c in &self.handlers_irc_cmd {
                if let Ok(true) = c(self, &message.command) {
                    break;
                }
            }

            match message.command {
                Command::Response(resp, v) => {
                    debug!("Got response type {resp:?} contents: {v:?}");
                }

                Command::PRIVMSG(channel, msg) => {
                    let (cmd, args) = match msg.split_once(|c: char| c.is_whitespace()) {
                        Some((c, a)) => (c, a),
                        None => (msg.as_str(), ""),
                    };

                    if channel == mynick {
                        if let Err(e) = self.handle_privmsg(msg.as_str(), cmd, args) {
                            error!("PRIVMSG handling failed: {e}");
                        }
                    } else if let Err(e) =
                        self.handle_chanmsg(&channel, msg.as_str(), cmd, args).await
                    {
                        error!("Channel msg handling failed: {e}");
                    }
                }

                Command::NICK(newnick) => {
                    debug!(
                        "NICK: {msg_nick} USER: {msg_user} HOST: {msg_host} NEW NICK: {newnick}"
                    );
                    if msg_nick == *mynick {
                        info!("My NEW nick: {newnick}");
                        self.mynick = newnick;
                    }
                }

                cmd => {
                    debug!("Unhandled command: {cmd:?}")
                }
            }
        }

        Ok(())
    }

    fn start_op_queue(&mut self) {
        let irc_sender = self.irc_sender.clone();
        let (tx, rx) = mpsc::unbounded_channel::<ModeOper>();
        self.op_sender = Some(Arc::new(tx));

        tokio::spawn(async move {
            read_op_queue(irc_sender, rx).await;
        });
    }

    fn mode(&self, mode: MyMode, channel: String, nick: String) -> anyhow::Result<bool> {
        info!("Giving +{mode:?} on {channel} to {nick}");
        let op_sender = self.op_sender.as_ref().unwrap().deref();
        op_sender.send(ModeOper {
            mode,
            channel,
            nick,
        })?;
        Ok(true)
    }

    pub fn mode_o<S1, S2>(&self, channel: S1, nick: S2) -> anyhow::Result<bool>
    where
        S1: AsRef<str> + Display,
        S2: AsRef<str> + Display,
    {
        self.mode(MyMode::Oper, channel.to_string(), nick.to_string())
    }

    pub fn mode_v<S1, S2>(&self, channel: S1, nick: S2) -> anyhow::Result<bool>
    where
        S1: AsRef<str> + Display,
        S2: AsRef<str> + Display,
    {
        self.mode(MyMode::Voice, channel.to_string(), nick.to_string())
    }

    // Process private messages here and return true only if something was reacted upon
    fn handle_privmsg(&mut self, msg: &str, cmd: &str, args: &str) -> anyhow::Result<bool> {
        let cfg = &self.bot_cfg;
        let nick = &self.msg_nick;
        let userhost = &self.msg_userhost();
        info!("*** Privmsg from {nick} ({userhost}): {cmd} {args}");

        if let Some(true) = cfg.privileged_nicks.get(nick) {
            // Handle privileged commands
            if self.handle_privmsg_priv(msg, cmd, args)? {
                // a command was found and executed if true was returned
                return Ok(true);
            }
        }

        // Handle public commands
        if self.handle_privmsg_open(msg, cmd, args)? {
            // a command was found and executed if true was returned
            return Ok(true);
        }

        // All other private messages are ignored
        Ok(false)
    }

    // Process privileged commands here and return true only if something was reacted upon
    fn handle_privmsg_priv(&mut self, msg: &str, cmd: &str, args: &str) -> anyhow::Result<bool> {
        match self.handlers_privmsg_priv.get(cmd) {
            Some(handler) => handler(self, msg, cmd, args),
            _ => Ok(false), // did not recognize any command
        }
    }

    // Process "public" commands here and return true only if something was reacted upon
    fn handle_privmsg_open(&mut self, msg: &str, cmd: &str, args: &str) -> anyhow::Result<bool> {
        match self.handlers_privmsg_open.get(cmd) {
            Some(handler) => handler(self, msg, cmd, args),
            _ => Ok(false), // did not recognize any command
        }
    }

    // Process channel messages here and return true only if something was reacted upon
    async fn handle_chanmsg(
        &mut self,
        channel: &str,
        msg: &str,
        cmd: &str,
        args: &str,
    ) -> anyhow::Result<bool> {
        let cfg = &self.bot_cfg;
        let nick = &self.msg_nick;
        debug!("{channel} <{nick}> {cmd} {args}");

        if let Some(handler) = self.handlers_chanmsg.get(cmd) {
            return handler(self, msg, cmd, args);
        }

        // url_cmd starts with '!'
        if let Some(u_cmd) = cmd.strip_prefix('!') {
            if let Some(c) = cfg.url_cmd_list.get(u_cmd) {
                // phew we found an url command to execute!

                let u_args = args.split_whitespace().collect::<Vec<&str>>();
                info!("Url cmd ctx arg: {args:?}");
                info!("Url cmd ctx args: {u_args:?}");

                // render URL to retrieve
                let mut ctx = tera::Context::new();
                ctx.insert("arg", args);
                ctx.insert("args", &u_args);
                info!("Url cmd ctx: {ctx:#?}");

                let url = cfg.url_cmd_tera.as_ref().unwrap().render(u_cmd, &ctx)?;
                info!("URL cmd: !{u_cmd} --> {url}");

                let client = reqwest::Client::builder()
                    .connect_timeout(time::Duration::new(5, 0))
                    .timeout(time::Duration::new(10, 0))
                    .danger_accept_invalid_certs(true)
                    .danger_accept_invalid_hostnames(true)
                    .min_tls_version(reqwest::tls::Version::TLS_1_0)
                    .user_agent(format!(
                        "{} v{}",
                        env!("CARGO_PKG_NAME"),
                        env!("CARGO_PKG_VERSION")
                    ))
                    .build()?;

                let body = client.get(&url).send().await?.text().await?;
                debug!("Got body:\n{body}");

                for res_cap in c.output_filter_re.as_ref().unwrap().captures_iter(&body) {
                    let res_str = &res_cap[1];
                    let say = format!("{u_cmd} --> {res_str}");
                    info!("{channel} <{mynick}> {say}", mynick = self.mynick);
                    self.irc.send_privmsg(&channel, say)?;
                }

                return Ok(true);
            }
        }

        // Are we supposed to detect urls and show titles on this channel?
        if let Some(true) = cfg.url_fetch_channels.get(channel) {
            let mut found_url = false;
            for url_cap in cfg
                .url_regex_re
                .as_ref()
                .unwrap()
                .captures_iter(msg.as_ref())
            {
                found_url = true;
                let url_s = &url_cap[1];
                if let Ok(url) = Url::parse(url_s) {
                    // Now we should have a canonical url, IDN handled etc.
                    let url_c = String::from(url);
                    info!("*** detected url: {url_c}");
                    info!("Fetching URL {url_c}");

                    let webpage_opts = WebpageOptions {
                        allow_insecure: true,
                        timeout: time::Duration::new(5, 0),
                        ..Default::default()
                    };

                    if let Ok(pageinfo) = Webpage::from_url(&url_c, webpage_opts) {
                        if let Some(title) = pageinfo.html.title {
                            // ignore titles that are just the url repeated
                            if title != url_s {
                                let say = format!("\"{title}\"");
                                info!("{channel} <{mynick}> {say}", mynick = self.mynick);
                                self.irc.send_privmsg(&channel, say)?;
                            }
                        }
                    }
                }
            }
            return Ok(found_url);
        }

        Ok(false)
    }
}

// We are throttling channel mode operations here
async fn read_op_queue(irc_sender: Arc<Sender>, mut rx: UnboundedReceiver<ModeOper>) {
    while let Some(o) = rx.recv().await {
        let op = match o.mode {
            MyMode::Oper => ChannelMode::Oper,
            MyMode::Voice => ChannelMode::Voice,
        };
        let channel = o.channel;
        let nick = o.nick;

        if let Err(e) = irc_sender.send_mode(channel, &[Mode::Plus(op, Some(nick))]) {
            error!("{e}");
        }
        sleep(Duration::from_secs(IRCMODE_RATE)).await;
    }
}
// EOF