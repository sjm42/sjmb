// ircbot.rs

use chrono_tz::Tz;
use futures::{future::BoxFuture, prelude::*};
use tera::Tera;

use crate::*;

const INITIAL_HANDLERS: usize = 8;

// in milliseconds
const IRC_OP_THROTTLE: u64 = 2500;
const IRC_MSG_THROTTLE: u64 = 1500;

pub type CmdHandler = Box<dyn Fn(Arc<IrcBot>, Command) -> BoxFuture<'static, anyhow::Result<bool>>>;

pub fn into_cmd_handler<Fut: Future<Output=anyhow::Result<bool>> + Send + 'static>(
    f: impl Fn(Arc<IrcBot>, Command) -> Fut + Send + 'static,
) -> CmdHandler {
    Box::new(move |bot, c| Box::pin(f(bot, c)))
}

pub type MsgHandler = Box<dyn Fn(Arc<IrcBot>, String, String, String) -> BoxFuture<'static, anyhow::Result<bool>>>;

pub fn into_msg_handler<Fut: Future<Output=anyhow::Result<bool>> + Send + 'static>(
    f: impl Fn(Arc<IrcBot>, String, String, String) -> Fut + Send + 'static,
) -> MsgHandler {
    Box::new(move |bot, a, b, c| Box::pin(f(bot, a, b, c)))
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UrlCmd {
    pub url_tmpl: String,
    // a Tera template string with {{arg}} if command needs an argument
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

    // dump my ACL as privmsgs
    pub cmd_dumpacl: String,
    // get /invite
    pub cmd_invite: String,
    // make bot join a channel
    pub cmd_join: String,
    // get +o
    pub cmd_mode_o: String,
    // get +v
    pub cmd_mode_v: String,
    // set nick of the bot
    pub cmd_nick: String,
    // reload config
    pub cmd_reload: String,
    // say something to a channel
    pub cmd_say: String,
    // Regex list for +o ACL
    pub mode_o_acl: Vec<String>,
    // Regex list for auto-op ACL
    pub auto_o_acl: Vec<String>,
    // Regex lists for blacklisted users
    pub invite_bl_userhost: Vec<String>,
    pub invite_bl_nick: Vec<String>,

    pub url_cmd_list: HashMap<String, UrlCmd>,
    pub url_mut_list: Vec<(String, String)>,

    #[serde(skip)]
    pub mode_o_acl_rt: Option<ReAcl>,
    #[serde(skip)]
    pub auto_o_acl_rt: Option<ReAcl>,
    #[serde(skip)]
    pub invite_bl_userhost_rt: Option<ReAcl>,
    #[serde(skip)]
    pub invite_bl_nick_rt: Option<ReAcl>,

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
    pub fn new(config_file: &str) -> anyhow::Result<Self> {
        let now1 = Utc::now();

        info!("Reading config file {config_file}");
        let mut config: BotConfig = serde_json::from_reader(BufReader::new(File::open(config_file)?))?;

        // Expand $HOME where relevant
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        config.url_log_db = shellexpand::full(&config.url_log_db)?.into_owned();

        // read & parse ACLs ()
        config.mode_o_acl_rt = Some(ReAcl::new(&config.mode_o_acl)?);
        config.auto_o_acl_rt = Some(ReAcl::new(&config.auto_o_acl)?);
        config.invite_bl_userhost_rt = Some(ReAcl::new(&config.invite_bl_userhost)?);
        config.invite_bl_nick_rt = Some(ReAcl::new(&config.invite_bl_nick)?);

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

pub struct BotHandlers {
    handlers_irc_cmd: Vec<CmdHandler>,
    handlers_privmsg_open: HashMap<String, MsgHandler>,
    handlers_privmsg_priv: HashMap<String, MsgHandler>,
    handlers_chanmsg: HashMap<String, MsgHandler>,
}

pub struct BotState {
    pub my_nick: String,
    pub msg_nick: String,
    pub msg_user: String,
    pub msg_host: String,
    pub msg_userhost: String,

    op_sender: mpsc::UnboundedSender<IrcOp>,
    msg_sender: mpsc::UnboundedSender<IrcMsg>,
}

pub struct IrcBot {
    pub cli_opts: RwLock<OptsCommon>,
    pub config: RwLock<BotConfig>,
    pub state: RwLock<BotState>,
    pub handlers: RwLock<BotHandlers>,
}

unsafe impl Send for IrcBot {}
unsafe impl Sync for IrcBot {}

impl IrcBot {
    pub async fn new(opts: &OptsCommon) -> anyhow::Result<(Self, irc::client::ClientStream)> {
        let bot_cfg = match BotConfig::new(&opts.bot_config) {
            Ok(b) => b,
            Err(e) => {
                bail!("{e}");
            }
        };

        let mut irc = match Client::new(&opts.irc_config).await {
            Ok(c) => c,
            Err(e) => {
                bail!("{e}");
            }
        };
        if let Err(e) = irc.identify() {
            bail!("{e}");
        }

        let my_nick = irc.current_nickname().to_string();
        let irc_sender1 = Arc::new(irc.sender());
        let irc_sender2 = irc_sender1.clone();

        let (op_sender, op_rx) = mpsc::unbounded_channel::<IrcOp>();
        tokio::spawn(async move {
            debug!("Starting op queue receiver");
            read_op_queue(irc_sender1, op_rx).await;
        });

        let (msg_sender, msg_rx) = mpsc::unbounded_channel::<IrcMsg>();
        tokio::spawn(async move {
            debug!("Starting msg queue receiver");
            read_msg_queue(irc_sender2, msg_rx).await;
        });

        Ok((
            IrcBot {
                cli_opts: RwLock::new(opts.clone()),
                config: RwLock::new(bot_cfg),
                state: RwLock::new(BotState {
                    my_nick,
                    msg_nick: "NONE".into(),
                    msg_user: "NONE".into(),
                    msg_host: "NONE".into(),
                    msg_userhost: "NONE@NONE".into(),

                    op_sender,
                    msg_sender,
                }),
                handlers: RwLock::new(BotHandlers {
                    handlers_irc_cmd: Vec::with_capacity(INITIAL_HANDLERS),
                    handlers_privmsg_open: HashMap::with_capacity(INITIAL_HANDLERS),
                    handlers_privmsg_priv: HashMap::with_capacity(INITIAL_HANDLERS),
                    handlers_chanmsg: HashMap::with_capacity(INITIAL_HANDLERS),
                }),
            },
            irc.stream()?,
        ))
    }

    pub async fn clear_handlers(&self) {
        let mut handlers = self.handlers.write().await;
        handlers.handlers_irc_cmd.clear();
        handlers.handlers_privmsg_open.clear();
        handlers.handlers_privmsg_priv.clear();
        handlers.handlers_chanmsg.clear();
    }

    pub async fn reload(&self) -> anyhow::Result<bool> {
        let config_file = self.cli_opts.read().await.bot_config.clone();
        match BotConfig::new(&config_file) {
            Ok(cfg) => {
                info!("*** Reload successful.");
                *self.config.write().await = cfg;
                Ok(true)
            }
            Err(e) => {
                error!("*** Reload failed.");
                let msg = format!("Could not parse runtime config {config_file}: {e}", );
                error!("{msg}");
                Err(anyhow!(msg))
            }
        }
    }

    pub async fn register_irc_cmd(&self, handler: CmdHandler) {
        self.handlers.write().await.handlers_irc_cmd.push(handler);
    }

    pub async fn register_privmsg_priv(&self, cmd: &str, handler: MsgHandler) {
        self.handlers
            .write()
            .await
            .handlers_privmsg_priv
            .insert(cmd.to_string(), handler);
    }

    pub async fn register_privmsg_open(&self, cmd: &str, handler: MsgHandler) {
        self.handlers
            .write()
            .await
            .handlers_privmsg_open
            .insert(cmd.to_string(), handler);
    }

    pub async fn register_chanmsg(&self, cmd: &str, handler: MsgHandler) {
        self.handlers
            .write()
            .await
            .handlers_chanmsg
            .insert(cmd.to_string(), handler);
    }

    pub async fn run(self: Arc<Self>, mut stream: irc::client::ClientStream) -> anyhow::Result<()> {
        while let Some(message) = stream.next().await.transpose()? {
            trace!("Got msg: {message:?}");

            let (msg_nick, msg_user, msg_host) = if let Some(Prefix::Nickname(nick, user, host)) = message.prefix {
                (nick, user, host)
            } else {
                ("NONE".into(), "NONE".into(), "NONE".into())
            };

            let my_nick = {
                let mut state = self.state.write().await;
                state.msg_nick = msg_nick.clone();
                state.msg_user = msg_user.clone();
                state.msg_host = msg_host.clone();
                state.msg_userhost = format!("{msg_user}@{msg_host}");
                state.my_nick.clone()
            };

            for c in self.handlers.read().await.handlers_irc_cmd.iter() {
                if let Ok(true) = c(self.clone(), message.command.clone()).await {
                    break;
                }
            }

            match message.command {
                Command::Response(resp, v) => {
                    debug!("Got response type {resp:?} contents: {v:?}");
                }

                Command::PRIVMSG(channel, msg) => {
                    let (cmd, args) = match msg.split_once(|c: char| c.is_whitespace()) {
                        Some((c, a)) => (c.to_string(), a.to_string()),
                        None => (msg.clone(), "".to_string()),
                    };

                    if channel == my_nick {
                        if let Err(e) = self.clone().handle_privmsg(msg, cmd, args).await {
                            error!("PRIVMSG handling failed: {e}");
                        }
                    } else if let Err(e) = self.clone().handle_chanmsg(channel, msg, cmd, args).await {
                        error!("CHANMSG handling failed: {e}");
                    }
                }

                Command::NICK(new_nick) => {
                    debug!("NICK: {msg_nick} USER: {msg_user} HOST: {msg_host} NEW NICK: {new_nick}");
                    if msg_nick == *my_nick {
                        info!("My NEW nick: {new_nick}");
                        self.state.write().await.my_nick = new_nick;
                    }
                }

                cmd => {
                    debug!("Unhandled command: {cmd:?}")
                }
            }
        }

        Ok(())
    }

    pub async fn new_op(self: Arc<Self>, op: IrcOp) -> anyhow::Result<bool> {
        debug!("new_op({op:?})");
        self.state.read().await.op_sender.send(op)?;
        debug!("new_op sent to queue");
        Ok(true)
    }

    pub async fn new_msg(self: Arc<Self>, target: &str, msg: &str) -> anyhow::Result<bool> {
        let (target_s, msg_s) = (target.to_string(), msg.to_string());
        let my_nick = self.state.read().await.my_nick.clone();
        info!("{target_s} <{my_nick}> {msg_s}");
        self.state.read().await.msg_sender.send(IrcMsg {
            target: target_s,
            msg: msg_s,
        })?;
        Ok(true)
    }

    // Process private messages here and return true only if something was reacted upon
    async fn handle_privmsg(self: Arc<Self>, msg: String, cmd: String, args: String) -> anyhow::Result<bool> {
        let nick = self.state.read().await.msg_nick.clone();
        let userhost = &self.state.read().await.msg_userhost.clone();
        info!("*** Privmsg from {nick} ({userhost}): {cmd} {args}");

        let nick_is_privileged = matches!(self.config.read().await.privileged_nicks.get(&nick), Some(true));

        if nick_is_privileged
            // Handle privileged commands
            && self.clone().handle_privmsg_priv(msg.clone(), cmd.clone(), args.clone()).await?
        {
            // a command was found and executed if true was returned
            return Ok(true);
        }

        // Handle public commands
        if self.handle_privmsg_open(msg, cmd, args).await? {
            // a command was found and executed if true was returned
            return Ok(true);
        }

        // All other private messages are ignored
        Ok(false)
    }

    // Process privileged commands here and return true only if something was reacted upon
    async fn handle_privmsg_priv(self: Arc<Self>, msg: String, cmd: String, args: String) -> anyhow::Result<bool> {
        match self.handlers.read().await.handlers_privmsg_priv.get(&cmd) {
            Some(handler) => handler(self.clone(), msg, cmd, args).await,
            _ => Ok(false), // did not recognize any command
        }
    }

    // Process "public" commands here and return true only if something was reacted upon
    async fn handle_privmsg_open(self: Arc<Self>, msg: String, cmd: String, args: String) -> anyhow::Result<bool> {
        match self.handlers.read().await.handlers_privmsg_open.get(&cmd) {
            Some(handler) => handler(self.clone(), msg, cmd, args).await,
            _ => Ok(false), // did not recognize any command
        }
    }

    // Process channel messages here and return true only if something was reacted upon
    async fn handle_chanmsg(
        self: Arc<Self>,
        channel: String,
        msg: String,
        cmd: String,
        args: String,
    ) -> anyhow::Result<bool> {
        let nick = self.state.read().await.msg_nick.clone();
        debug!("{channel} <{nick}> {cmd} {args}");

        if let Some(handler) = self.handlers.read().await.handlers_chanmsg.get(&cmd) {
            return handler(self.clone(), msg, cmd, args).await;
        }

        let cfg = self.config.read().await;

        // url_cmd starts with '!'
        if let (Some(u_cmd), Some(true)) = (cmd.strip_prefix('!'), get_wild(&cfg.url_cmd_channels, &channel))
            && let Some(c) = cfg.url_cmd_list.get(u_cmd)
        {
            // phew we found an url command to execute!

            let u_args = args.split_whitespace().collect::<Vec<&str>>();
            debug!("Url cmd ctx arg: {args:?}");
            debug!("Url cmd ctx args: {u_args:?}");

            // render URL to retrieve
            let mut ctx = tera::Context::new();
            ctx.insert("arg", &args);
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
            self.clone()
                .new_op(IrcOp::UrlFetch(url, channel.to_string(), f))
                .await?;
            return Ok(true);
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
            if let Some(true) = get_wild(&cfg.url_log_channels, &channel) {
                let db = cfg.url_log_db.clone();

                // Are we supposed to complain about duplicate urls on this channel?
                if let Some(true) = get_wild(&cfg.url_dup_complain_channels, &channel) {
                    let expire_days = get_wild(&cfg.url_dup_expire_days, &channel).unwrap_or(&7);
                    // Which timezone does this channel want? Defaulting to UTC.
                    let tz = get_wild(cfg.url_dup_tz.as_ref().unwrap(), &channel).unwrap_or(&Tz::UTC);
                    self.clone()
                        .new_op(IrcOp::UrlCheck(
                            db.clone(),
                            url_s.clone(),
                            channel.to_owned(),
                            tz.to_owned(),
                            expire_days.to_owned(),
                        ))
                        .await?;
                }

                let op = IrcOp::UrlLog(
                    db,
                    url_s.clone(),
                    channel.to_owned(),
                    nick.to_owned(),
                    Utc::now().timestamp(),
                );
                debug!("New op: {op:?}");
                self.clone().new_op(op).await?;
            }

            // Are we supposed to detect urls and show titles on this channel?
            if let Some(true) = get_wild(&cfg.url_fetch_channels, &channel) {
                self.clone()
                    .new_op(IrcOp::UrlTitle(url_s.clone(), channel.to_owned()))
                    .await?;
            }

            // Are we supposed to mutate some urls on this channel?
            if let Some(true) = get_wild(&cfg.url_mut_channels, &channel)
                && let Some((_i, new_url)) = cfg
                .url_mut_re
                .as_ref()
                .ok_or_else(|| anyhow!("No url_mut_re"))?
                .re_mut(&url_s)
            {
                debug!("Doing url mut");
                self.clone().new_msg(&channel, new_url.as_str()).await?;
                self.clone()
                    .new_op(IrcOp::UrlTitle(new_url, channel.to_string()))
                    .await?;
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
async fn read_msg_queue(irc_sender: Arc<Sender>, mut rx: mpsc::UnboundedReceiver<IrcMsg>) {
    while let Some(m) = rx.recv().await {
        debug!("read_msg_queue: new msg: {m:?}");
        let (target, msg) = (m.target, m.msg);
        if let Err(e) = irc_sender.send_privmsg(target, msg) {
            error!("{e}");
        }
        sleep(Duration::from_millis(IRC_MSG_THROTTLE)).await;
    }
}

// We are throttling operations (mode/join/invite/nick etc) here
async fn read_op_queue(irc_sender: Arc<Sender>, mut rx: mpsc::UnboundedReceiver<IrcOp>) {
    while let Some(op) = rx.recv().await {
        debug!("read_op_queue: new op: {op:?}");
        let res = op_dispatch(irc_sender.clone(), op).await;
        if let Err(e) = res {
            error!("{e}");
        }
        sleep(Duration::from_millis(IRC_OP_THROTTLE)).await;
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
        IrcOp::UrlCheck(db, url, channel, tz, days) => {
            op_handle_urlcheck(irc_sender.clone(), db, url, channel, tz, days).await?
        }
        IrcOp::UrlFetch(url, channel, output_filter) => {
            op_handle_urlfetch(irc_sender.clone(), url, channel, output_filter).await?
        }
        IrcOp::UrlLog(db, url, channel, nick, ts) => op_handle_urllog(db, url, channel, nick, ts).await?,
        IrcOp::UrlTitle(url, channel) => op_handle_urltitle(irc_sender.clone(), url, channel).await?,
    }
    Ok(())
}

async fn op_handle_urlcheck(
    irc_sender: Arc<Sender>,
    db: String,
    url: String,
    channel: String,
    tz: Tz,
    exp_days: i64,
) -> anyhow::Result<()> {
    debug!("op_handle_urlcheck(): url {url}");
    let dbc = start_db(&db).await?;
    debug!("op_handle_urlcheck(): db connected");

    if let Some(old) = db_check_url(&dbc, &url, &channel, exp_days * 86400).await?
        && let (Some(first), Some(last)) = (old.first, old.last)
    {
        let ts_first = DateTime::from_timestamp(first, 0)
            .unwrap_or_default()
            .with_timezone(&tz);

        match old.cnt.cmp(&1) {
            Ordering::Equal => {
                irc_sender.send_privmsg(channel, format!("Wanha URL, nähty {ts_first}"))?;
            }
            Ordering::Greater => {
                let ts_last = DateTime::from_timestamp(last, 0).unwrap_or_default().with_timezone(&tz);
                irc_sender.send_privmsg(
                    channel,
                    format!(
                        "Wanha URL, nähty {} kertaa, ensin {ts_first} ja viimeksi {ts_last}",
                        old.cnt
                    ),
                )?;
            }
            _ => {}
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
    debug!("op_handle_urlfetch()");
    if let Some((body, _ct)) = get_text_body(&url).await? {
        for res_cap in output_filter.captures_iter(&body) {
            let res_str = &res_cap[1];
            let say = format!("--> {res_str}");
            irc_sender.send_privmsg(&channel, say)?;
        }
    }
    Ok(())
}

async fn op_handle_urllog(db: String, url: String, chan: String, nick: String, ts: i64) -> anyhow::Result<()> {
    debug!("op_handle_urllog(): insert url {url}");
    let dbc = start_db(&db).await?;
    info!(
        "Urllog: inserted {} row(s)",
        db_add_url(&dbc, &UrlCtx { ts, chan, nick, url },).await?
    );
    Ok(())
}

async fn op_handle_urltitle(irc_sender: Arc<Sender>, url: String, channel: String) -> anyhow::Result<()> {
    debug!("op_handle_urltitle(): fetching url {url}");
    if let Some((body, _ct)) = get_text_body(&url).await? {
        debug!("Parsing title from body: url {url}");
        let html = webpage::HTML::from_string(body, None)?;

        if let Some(title) = html.title
            // ignore titles that are just the url repeated
            && title != url
        {
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
                title_c = format!("{s1}...");
            }
            let say = format!("\"{title_c}\"");
            irc_sender.send_privmsg(channel, say)?;
        }
    }
    Ok(())
}
// EOF
