// ircbot.rs

use anyhow::{anyhow, bail};
use chrono::*;
use chrono_tz::Tz;
use futures::prelude::*;
use irc::client::prelude::*;
use log::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt::Display, fs::File, io::BufReader, sync::Arc};
use tera::Tera;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{sleep, Duration};

#[cfg(feature = "sqlite")]
use std::cmp::Ordering;

use crate::*;

const INITIAL_SIZE: usize = 32;
const IRC_OP_THROTTLE: u64 = 3; // in seconds
const IRC_MSG_THROTTLE: u64 = 2; // in seconds

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

    pub url_regex: String,
    pub url_log_db: String,
    pub url_blacklist: Vec<String>,

    pub url_fetch_channels: HashMap<String, bool>,
    pub url_cmd_channels: HashMap<String, bool>,
    pub url_mut_channels: HashMap<String, bool>,
    pub url_log_channels: HashMap<String, bool>,
    pub url_dup_complain_channels: HashMap<String, bool>,
    pub url_dup_expire_days: HashMap<String, i64>,
    pub url_dup_timezone: HashMap<String, String>,

    pub cmd_dumpacl: String, // dump my ACL as privmsgs
    pub cmd_invite: String,  // get /invite
    pub cmd_join: String,    // make bot join a channel
    pub cmd_mode_o: String,  // get +o
    pub cmd_mode_v: String,  // get +v
    pub cmd_nick: String,    // set nick of the bot
    pub cmd_reload: String,  // reload config
    pub cmd_say: String,     // say something to a channel

    pub mode_o_acl: Vec<String>, // Regex list for +o ACL
    pub auto_o_acl: Vec<String>, // Regex list for auto-op ACL

    pub url_cmd_list: HashMap<String, UrlCmd>,
    pub url_mut_list: Vec<(String, String)>,

    #[serde(skip)]
    pub mode_o_acl_rt: Option<ReAcl>,
    #[serde(skip)]
    pub auto_o_acl_rt: Option<ReAcl>,

    #[serde(skip)]
    pub url_re: Option<Regex>,
    #[serde(skip)]
    pub url_cmd_tera: Option<Tera>,
    #[serde(skip)]
    pub url_mut_re: Option<ReMut>,
    #[serde(skip)]
    pub url_dup_tz: Option<HashMap<String, Tz>>,
}

impl BotConfig {
    pub fn new(opts: &OptsCommon) -> anyhow::Result<Self> {
        let now1 = Utc::now();

        let file = &opts.bot_config;
        info!("Reading config file {file}");
        let mut config: BotConfig = serde_json::from_reader(BufReader::new(File::open(file)?))?;

        // Expand $HOME where relevant
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        config.url_log_db = shellexpand::full(&config.url_log_db)?.into_owned();

        // read & parse ACLs ()
        config.mode_o_acl_rt = Some(ReAcl::new(&config.mode_o_acl)?);
        config.auto_o_acl_rt = Some(ReAcl::new(&config.auto_o_acl)?);

        // pre-compile url detection regex
        config.url_re = Some(Regex::new(&config.url_regex)?);

        // prepare url-based commands, if any
        let mut tera = Tera::default();
        for (k, c) in config.url_cmd_list.iter_mut() {
            tera.add_raw_template(k, &c.url_tmpl)?;
            c.output_filter_re = Some(Regex::new(&c.output_filter)?);
        }
        config.url_cmd_tera = Some(tera);

        // Prepare Url mutation list
        config.url_mut_re = Some(ReMut::new(&config.url_mut_list)?);

        let mut url_dup_tz = HashMap::new();
        // Parse the timezones
        for (k, v) in &config.url_dup_timezone {
            match v.as_str().parse::<Tz>() {
                Ok(tz) => {
                    url_dup_tz.insert(k.to_string(), tz);
                }
                Err(e) => {
                    bail!("error parsing url_dup_timezone \"{k}\": \"{v}\" - {e}");
                }
            }
        }
        config.url_dup_tz = Some(url_dup_tz);

        info!(
            "New runtime config successfully created in {} ms.",
            Utc::now().signed_duration_since(now1).num_milliseconds()
        );
        debug!("New BotConfig:\n{config:#?}");

        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub enum IrcOp {
    ModeVoice(String, String),
    ModeOper(String, String),
    Invite(String, String),
    Nick(String),
    Join(String),
    UrlCheck(String, String, String, Tz, i64),
    UrlTitle(String, String),
    UrlLog(String, String, String, String, i64),
    UrlFetch(String, String, Regex),
}

#[derive(Debug, Clone)]
struct IrcMsg {
    target: String,
    msg: String,
}

pub struct IrcBot {
    irc: Client,
    irc_sender: Arc<Sender>,
    opts: OptsCommon,
    pub bot_cfg: BotConfig,
    mynick: String,
    msg_nick: String,
    msg_user: String,
    msg_host: String,
    msg_userhost: String,

    op_sender: Option<UnboundedSender<IrcOp>>,
    msg_sender: Option<UnboundedSender<IrcMsg>>,
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
                bail!("{e}");
            }
        };

        let irc = match Client::new(&opts.irc_config).await {
            Ok(c) => c,
            Err(e) => {
                bail!("{e}");
            }
        };
        if let Err(e) = irc.identify() {
            bail!("{e}");
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
            msg_sender: None,
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
                Err(anyhow!(msg))
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
        self.start_msg_queue();

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
                        if let Err(e) = self.handle_privmsg(msg.as_str(), cmd, args).await {
                            error!("PRIVMSG handling failed: {e}");
                        }
                    } else if let Err(e) =
                        self.handle_chanmsg(&channel, msg.as_str(), cmd, args).await
                    {
                        error!("CHANMSG handling failed: {e}");
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
        let (tx, rx) = mpsc::unbounded_channel::<IrcOp>();
        self.op_sender = Some(tx);

        tokio::spawn(async move {
            read_op_queue(irc_sender, rx).await;
        });
    }

    fn start_msg_queue(&mut self) {
        let irc_sender = self.irc_sender.clone();
        let (tx, rx) = mpsc::unbounded_channel::<IrcMsg>();
        self.msg_sender = Some(tx);

        tokio::spawn(async move {
            read_msg_queue(irc_sender, rx).await;
        });
    }

    pub fn new_op(&self, op: IrcOp) -> anyhow::Result<bool> {
        let op_sender = self
            .op_sender
            .as_ref()
            .ok_or_else(|| anyhow!("No sender"))?;
        op_sender.send(op)?;
        Ok(true)
    }

    pub fn new_msg<S1, S2>(&self, target: S1, msg: S2) -> anyhow::Result<()>
    where
        S1: AsRef<str> + Display,
        S2: AsRef<str> + Display,
    {
        let (target_s, msg_s) = (target.to_string(), msg.to_string());
        let mynick = &self.mynick;
        info!("{target_s} <{mynick}> {msg_s}");
        let msg_sender = self
            .msg_sender
            .as_ref()
            .ok_or_else(|| anyhow!("No sender"))?;
        msg_sender.send(IrcMsg {
            target: target_s,
            msg: msg_s,
        })?;
        Ok(())
    }

    // Process private messages here and return true only if something was reacted upon
    async fn handle_privmsg(&mut self, msg: &str, cmd: &str, args: &str) -> anyhow::Result<bool> {
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
        if let (Some(u_cmd), Some(true)) = (
            cmd.strip_prefix('!'),
            get_wild(&cfg.url_cmd_channels, channel),
        ) {
            if let Some(c) = cfg.url_cmd_list.get(u_cmd) {
                // phew we found an url command to execute!

                let u_args = args.split_whitespace().collect::<Vec<&str>>();
                debug!("Url cmd ctx arg: {args:?}");
                debug!("Url cmd ctx args: {u_args:?}");

                // render URL to retrieve
                let mut ctx = tera::Context::new();
                ctx.insert("arg", args);
                ctx.insert("args", &u_args);
                debug!("Url cmd ctx: {ctx:#?}");

                let url = cfg
                    .url_cmd_tera
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No tera"))?
                    .render(u_cmd, &ctx)?;
                info!("URL cmd: !{u_cmd} --> {url}");
                let f = c
                    .output_filter_re
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No output regex"))?
                    .clone();
                self.new_op(IrcOp::UrlFetch(url, channel.to_string(), f))?;
                return Ok(true);
            }
        }

        let mut found_url = false;
        'outer: for url_cap in cfg
            .url_re
            .as_ref()
            .ok_or_else(|| anyhow!("No url_regex_re"))?
            .captures_iter(msg.as_ref())
        {
            found_url = true;
            let url_s = url_cap[1].to_string();
            info!("*** ({nick} at {channel}) detected url: {url_s}");

            for b in &cfg.url_blacklist {
                if url_s.starts_with(b) {
                    info!("*** Blacklilsted URL. Ignored.");
                    continue 'outer;
                }
            }

            // Are we supposed to log urls on this channel?
            if let Some(true) = get_wild(&cfg.url_log_channels, channel) {
                let db = cfg.url_log_db.clone();

                // Are we supposed to complain about duplicate urls on this channel?
                if let Some(true) = get_wild(&cfg.url_dup_complain_channels, channel) {
                    let expire_days = get_wild(&cfg.url_dup_expire_days, channel).unwrap_or(&7);
                    // Which timezone does this channel want? Defaulting to UTC.
                    let tz =
                        get_wild(cfg.url_dup_tz.as_ref().unwrap(), channel).unwrap_or(&Tz::UTC);
                    self.new_op(IrcOp::UrlCheck(
                        db.clone(),
                        url_s.clone(),
                        channel.to_owned(),
                        tz.to_owned(),
                        expire_days.to_owned(),
                    ))?;
                }

                self.new_op(IrcOp::UrlLog(
                    db,
                    url_s.clone(),
                    channel.to_owned(),
                    nick.to_owned(),
                    Utc::now().timestamp(),
                ))?;
            }

            // Are we supposed to detect urls and show titles on this channel?
            if let Some(true) = get_wild(&cfg.url_fetch_channels, channel) {
                self.new_op(IrcOp::UrlTitle(url_s.clone(), channel.to_owned()))?;
            }

            // Are we supposed to mutate some urls on this channel?
            if let Some(true) = get_wild(&cfg.url_mut_channels, channel) {
                debug!("Checking url mut");
                if let Some((_i, new_url)) = cfg
                    .url_mut_re
                    .as_ref()
                    .ok_or_else(|| anyhow!("No url_mut_re"))?
                    .re_mut(&url_s)
                {
                    self.new_msg(channel, new_url.as_str())?;
                    self.new_op(IrcOp::UrlTitle(new_url, channel.to_string()))?;
                }
            }
        }

        // more processing might happen here
        if found_url {
            return Ok(true);
        }
        // ...or here

        Ok(false)
    }
}

// We are throttling messages here
async fn read_msg_queue(irc_sender: Arc<Sender>, mut rx: UnboundedReceiver<IrcMsg>) {
    while let Some(m) = rx.recv().await {
        let (target, msg) = (m.target, m.msg);
        if let Err(e) = irc_sender.send_privmsg(target, msg) {
            error!("{e}");
        }
        sleep(Duration::from_secs(IRC_MSG_THROTTLE)).await;
    }
}

// We are throttling operations (mode/join/invite/nick etc) here
async fn read_op_queue(irc_sender: Arc<Sender>, mut rx: UnboundedReceiver<IrcOp>) {
    while let Some(op) = rx.recv().await {
        let res = op_dispatch(irc_sender.clone(), op).await;
        if let Err(e) = res {
            error!("{e}");
        }
        sleep(Duration::from_secs(IRC_OP_THROTTLE)).await;
    }
}

#[allow(unused_variables)]
async fn op_dispatch(irc_sender: Arc<Sender>, op: IrcOp) -> anyhow::Result<()> {
    match op {
        IrcOp::Invite(nick, channel) => irc_sender.send_invite(nick, channel)?,
        IrcOp::Join(newchan) => irc_sender.send(Command::JOIN(newchan, None, None))?,
        IrcOp::ModeOper(channel, nick) => {
            irc_sender.send_mode(channel, &[Mode::Plus(ChannelMode::Oper, Some(nick))])?
        }
        IrcOp::ModeVoice(channel, nick) => {
            irc_sender.send_mode(channel, &[Mode::Plus(ChannelMode::Voice, Some(nick))])?
        }
        IrcOp::Nick(newnick) => irc_sender.send(Command::NICK(newnick))?,
        IrcOp::UrlCheck(db, url, channel, tz, days) =>
        {
            #[cfg(feature = "sqlite")]
            op_handle_urlcheck(irc_sender.clone(), db, url, channel, tz, days).await?
        }
        IrcOp::UrlFetch(url, channel, output_filter) => {
            op_handle_urlfetch(irc_sender.clone(), url, channel, output_filter).await?
        }
        IrcOp::UrlLog(db, url, channel, nick, ts) =>
        {
            #[cfg(feature = "sqlite")]
            op_handle_urllog(db, url, channel, nick, ts).await?
        }
        IrcOp::UrlTitle(url, channel) => {
            op_handle_urltitle(irc_sender.clone(), url, channel).await?
        }
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
async fn op_handle_urlcheck(
    irc_sender: Arc<Sender>,
    db: String,
    url: String,
    channel: String,
    tz: Tz,
    exp_days: i64,
) -> anyhow::Result<()> {
    let mut dbc = start_db(&db).await?;
    if let Some(old) = db_check_url(&mut dbc, &url, &channel, exp_days * 86400).await? {
        let ts_min = tz.from_utc_datetime(
            &NaiveDateTime::from_timestamp_opt(old.min, 0).ok_or(anyhow!("timestamp error"))?,
        );

        let msg = match old.cnt.cmp(&1) {
            Ordering::Equal => {
                format!("Wanha URL, nähty {ts_min}")
            }
            Ordering::Greater => {
                let ts_max = tz.from_utc_datetime(
                    &NaiveDateTime::from_timestamp_opt(old.max, 0)
                        .ok_or(anyhow!("timestamp error"))?,
                );

                format!(
                    "Wanha URL, nähty {} kertaa, ensin {ts_min} ja viimeksi {ts_max}",
                    old.cnt
                )
            }
            _ => "".to_string(),
        };
        if !msg.is_empty() {
            irc_sender.send_privmsg(channel, &msg)?;
        }
    }

    Ok(())
}

async fn op_handle_urlfetch(
    irc_sender: Arc<Sender>,
    url: String,
    channel: String,
    output_filter: Regex,
) -> anyhow::Result<()> {
    let body = get_url_body(&url).await?;
    for res_cap in output_filter.captures_iter(&body) {
        let res_str = &res_cap[1];
        let say = format!("--> {res_str}");
        irc_sender.send_privmsg(&channel, say)?;
    }

    Ok(())
}

#[cfg(feature = "sqlite")]
async fn op_handle_urllog(
    db: String,
    url: String,
    chan: String,
    nick: String,
    ts: i64,
) -> anyhow::Result<()> {
    let mut dbc = start_db(&db).await?;
    info!(
        "Urllog: inserted {} row(s)",
        db_add_url(
            &mut dbc,
            &UrlCtx {
                ts,
                chan,
                nick,
                url,
            },
        )
        .await?
    );
    Ok(())
}


async fn op_handle_urltitle(
    irc_sender: Arc<Sender>,
    url: String,
    channel: String,
) -> anyhow::Result<()> {
    let html = webpage::HTML::from_string(get_url_body(&url).await?, None)?;
    if let Some(title) = html.title {
        // ignore titles that are just the url repeated
        if title != url {
            // Replace all consecutive whitespace, including newlines etc with a single space
            let mut title_c = title.ws_collapse();
            if title_c.len() > 400 {
                let mut i = 396;
                loop {
                    // find a UTF-8 code point boundary to safely split at
                    if title_c.is_char_boundary(i) {
                        break;
                    }
                    i += 1;
                }
                let (s1, _) = title_c.split_at(i);
                title_c = format!("{}...", s1);
            }
            let say = format!("\"{title_c}\"");
            irc_sender.send_privmsg(channel, say)?;
        }
    }
    Ok(())
}

// EOF
