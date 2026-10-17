// ircbot.rs

use futures::{future::BoxFuture, prelude::*};
use irc::client::prelude::{ChannelMode, Client, Message, Mode, Prefix, Response, Sender};

use crate::*;

const INITIAL_HANDLERS: usize = 8;

// in milliseconds
const IRC_OP_THROTTLE: u64 = 2500;
const IRC_OP_THROTTLE_JITTER: u64 = 1000;
const IRC_MSG_THROTTLE: u64 = 1500;
const IRC_QUEUE_CAPACITY: usize = 42;
const MAX_URLS_PER_MESSAGE: usize = 4;
const MAX_FETCH_REPLIES: usize = 4;
const OUTPUT_BATCH: usize = 16;
const OUTPUT_ACK_TIMEOUT: Duration = Duration::from_secs(20);
const IRC_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_QUEUE_AGE: Duration = Duration::from_secs(180);

task_local! {
    // Keep the original request's deadline when it creates operations or replies.
    static REQUEST_DEADLINE: Instant;
}

struct Queued<T> {
    expires_at: Instant,
    item: T,
}

pub type CmdHandler = Arc<dyn Fn(Arc<IrcBot>, Command) -> BoxFuture<'static, anyhow::Result<bool>> + Send + Sync>;

pub fn into_cmd_handler<Fut: Future<Output = anyhow::Result<bool>> + Send + 'static>(
    f: impl Fn(Arc<IrcBot>, Command) -> Fut + Send + Sync + 'static,
) -> CmdHandler {
    Arc::new(move |bot, c| Box::pin(f(bot, c)))
}

pub type MsgHandler =
    Arc<dyn Fn(Arc<IrcBot>, String, String, String) -> BoxFuture<'static, anyhow::Result<bool>> + Send + Sync>;

pub fn into_msg_handler<Fut: Future<Output = anyhow::Result<bool>> + Send + 'static>(
    f: impl Fn(Arc<IrcBot>, String, String, String) -> Fut + Send + Sync + 'static,
) -> MsgHandler {
    Arc::new(move |bot, a, b, c| Box::pin(f(bot, a, b, c)))
}

#[derive(Debug, Clone)]
pub enum IrcOp {
    ModeVoice(String, String),
    ModeOper(String, String),
    Invite(String, String),
    Nick(String),
    Join(String),
    UrlCheck(DbCtx, String, String, Tz, i64),
    UrlTitle(String, String),
    UrlLog(DbCtx, String, String, String, i64),
    UrlFetch(String, String, Regex),
    Reload(String),
}

#[derive(Debug, Clone)]
struct IrcMsg {
    target: String,
    msg: String,
}

#[derive(Debug, Clone)]
struct Membership {
    channel: String,
    nick: String,
    identity: Arc<()>,
}

#[derive(Debug)]
struct OpEnvelope {
    op: IrcOp,
    membership: Option<Membership>,
}

#[derive(Debug)]
enum IrcOutput {
    Msg(IrcMsg),
    Op(Command, Option<Membership>),
}

struct Incoming {
    message: Message,
    my_nick: String,
    memberships: HashMap<String, Membership>,
}

struct WorkerQueues {
    ops: mpsc::Receiver<Queued<OpEnvelope>>,
    output: mpsc::Receiver<Queued<IrcOutput>>,
    sender: Sender,
}

#[derive(Default)]
struct OutputGate {
    sent: usize,
    waiting: Option<(String, Instant)>,
}

impl OutputGate {
    fn acknowledge(&mut self, command: &Command) {
        if let Command::PONG(first, second) = command
            && self
                .waiting
                .as_ref()
                .is_some_and(|(token, _)| first == token || second.as_ref() == Some(token))
        {
            self.waiting = None;
            self.sent = 0;
        }
    }
}

#[derive(Debug, Clone)]
enum QueuedAction {
    Msg(IrcMsg),
    Op(IrcOp),
}

#[derive(Debug, Clone, Default)]
struct ChannelUserModes {
    identity: Arc<()>,
    founder: bool,
    admin: bool,
    oper: bool,
    halfop: bool,
    voice: bool,
}

impl ChannelUserModes {
    fn merge(&mut self, other: &Self) {
        self.founder |= other.founder;
        self.admin |= other.admin;
        self.oper |= other.oper;
        self.halfop |= other.halfop;
        self.voice |= other.voice;
    }

    fn set(&mut self, mode: &ChannelMode, enabled: bool) {
        match mode {
            ChannelMode::Founder => self.founder = enabled,
            ChannelMode::Admin => self.admin = enabled,
            ChannelMode::Oper => self.oper = enabled,
            ChannelMode::Halfop => self.halfop = enabled,
            ChannelMode::Voice => self.voice = enabled,
            _ => {}
        }
    }

    fn set_prefix(&mut self, prefix: char) -> bool {
        match prefix {
            '~' => self.founder = true,
            '&' => self.admin = true,
            '@' => self.oper = true,
            '%' => self.halfop = true,
            '+' => self.voice = true,
            _ => return false,
        }
        true
    }
}

fn is_tracked_user_mode(mode: &ChannelMode) -> bool {
    matches!(
        mode,
        ChannelMode::Founder | ChannelMode::Admin | ChannelMode::Oper | ChannelMode::Halfop | ChannelMode::Voice
    )
}

#[derive(Debug, Default)]
struct ChannelModes {
    case_mapping: CaseMapping,
    users: HashMap<String, HashMap<String, ChannelUserModes>>,
}

impl ChannelModes {
    fn set_case_mapping(&mut self, mapping: CaseMapping) -> anyhow::Result<()> {
        if mapping != self.case_mapping && !self.users.is_empty() {
            bail!("IRC CASEMAPPING changed after channel tracking began; reconnecting to rebuild state");
        }
        self.case_mapping = mapping;
        Ok(())
    }

    fn memberships(&self, nick: &str) -> HashMap<String, Membership> {
        self.users
            .keys()
            .filter_map(|channel| self.membership(channel, nick).map(|member| (channel.clone(), member)))
            .collect()
    }

    fn has_oper(&self, channel: &str, nick: &str) -> bool {
        self.users
            .get(&self.case_mapping.fold(channel))
            .and_then(|users| users.get(&self.case_mapping.fold(nick)))
            .is_some_and(|modes| modes.oper)
    }

    fn membership(&self, channel: &str, nick: &str) -> Option<Membership> {
        let channel = self.case_mapping.fold(channel);
        let nick = self.case_mapping.fold(nick);
        self.users.get(&channel)?.get(&nick).map(|user| Membership {
            channel: channel.clone(),
            nick: nick.clone(),
            identity: user.identity.clone(),
        })
    }

    fn current(&self, member: &Membership) -> bool {
        self.users
            .get(&member.channel)
            .and_then(|users| users.get(&member.nick))
            .is_some_and(|user| Arc::ptr_eq(&user.identity, &member.identity))
    }

    fn join(&mut self, channel: &str, nick: &str) {
        if nick.is_empty() || nick == "NONE" {
            return;
        }
        self.users
            .entry(self.case_mapping.fold(channel))
            .or_default()
            .insert(self.case_mapping.fold(nick), ChannelUserModes::default());
    }

    fn part(&mut self, channel: &str, nick: &str, my_nick: &str) {
        let channel = self.case_mapping.fold(channel);
        if self.case_mapping.eq(nick, my_nick) {
            self.users.remove(&channel);
        } else if let Some(users) = self.users.get_mut(&channel) {
            users.remove(&self.case_mapping.fold(nick));
        }
    }

    fn kick(&mut self, channel: &str, nick: &str, my_nick: &str) {
        self.part(channel, nick, my_nick);
    }

    fn quit(&mut self, nick: &str) {
        let nick = self.case_mapping.fold(nick);
        for users in self.users.values_mut() {
            users.remove(&nick);
        }
    }

    fn nick_change(&mut self, old_nick: &str, new_nick: &str) {
        if old_nick.is_empty() || old_nick == "NONE" || new_nick.is_empty() {
            return;
        }
        let old_nick = self.case_mapping.fold(old_nick);
        let new_nick = self.case_mapping.fold(new_nick);
        for users in self.users.values_mut() {
            if let Some(mut modes) = users.remove(&old_nick) {
                // Even a case-only change invalidates already queued grants.
                modes.identity = Arc::new(());
                users.insert(new_nick.clone(), modes);
            }
        }
    }

    fn apply_modes(&mut self, channel: &str, modes: &[Mode<ChannelMode>]) {
        let channel = self.case_mapping.fold(channel);
        for mode in modes {
            match mode {
                Mode::Plus(channel_mode, Some(nick)) if is_tracked_user_mode(channel_mode) => {
                    self.users
                        .entry(channel.clone())
                        .or_default()
                        .entry(self.case_mapping.fold(nick))
                        .or_default()
                        .set(channel_mode, true);
                }
                Mode::Minus(channel_mode, Some(nick)) if is_tracked_user_mode(channel_mode) => {
                    if let Some(users) = self.users.get_mut(&channel)
                        && let Some(user) = users.get_mut(&self.case_mapping.fold(nick))
                    {
                        user.set(channel_mode, false);
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_namreply(&mut self, args: &[String]) {
        if args.len() != 4 {
            return;
        }
        let users = self.users.entry(self.case_mapping.fold(&args[2])).or_default();
        for name in args[3].split_whitespace() {
            let (nick, modes) = parse_namreply_user(name);
            if !nick.is_empty() {
                // A new entry always owns a distinct identity, including NAMES/MODE paths.
                users.entry(self.case_mapping.fold(&nick)).or_default().merge(&modes);
            }
        }
    }
}

fn parse_namreply_user(name: &str) -> (String, ChannelUserModes) {
    let mut modes = ChannelUserModes::default();
    let mut nick_start = 0;

    for (i, c) in name.char_indices() {
        if !modes.set_prefix(c) {
            nick_start = i;
            break;
        }
        nick_start = i + c.len_utf8();
    }

    (name[nick_start..].to_string(), modes)
}

fn welcome_nickname(command: &Command) -> Option<&str> {
    match command {
        Command::Response(Response::RPL_WELCOME, args) => {
            args.first().map(String::as_str).filter(|nick| !nick.is_empty())
        }
        _ => None,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UrlCmd {
    pub url_tmpl: String,
    // a Tera template string with `arg` and `args` in its context
    pub output_filter: String,
    #[serde(skip)]
    pub output_filter_re: Option<Regex>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BotConfig {
    pub irc_log_dir: String,
    pub channel: String,
    pub privileged_nicks: HashMap<String, bool>,
    /// Optional per-nickname user@host wildcard masks. Missing entries preserve legacy nick-only access.
    #[serde(default)]
    pub privileged_userhosts: HashMap<String, Vec<String>>,
    #[serde(skip)]
    privileged_userhosts_rt: HashMap<String, Vec<Regex>>,

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
    // show the bot version
    #[serde(default = "default_version_command")]
    pub cmd_version: String,
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
    #[serde(skip)]
    pub db: Option<DbCtx>,
}
fn default_version_command() -> String {
    "version".into()
}

impl BotConfig {
    pub fn new(config_file: &str) -> anyhow::Result<Self> {
        let now1 = Utc::now();

        info!("Reading config file {config_file}");
        let mut config: BotConfig = serde_json::from_slice(&read_config_file(config_file)?)?;

        config.validate()?;
        for (nick, masks) in &config.privileged_userhosts {
            config.privileged_userhosts_rt.insert(
                nick.clone(),
                masks.iter().map(|s| wildcard_regex(s)).collect::<anyhow::Result<_>>()?,
            );
        }

        // Expand $HOME where relevant
        config.irc_log_dir = shellexpand::full(&config.irc_log_dir)?.into_owned();
        config.url_log_db = shellexpand::full(&config.url_log_db)?.into_owned();
        config
            .url_log_db
            .parse::<sqlx::postgres::PgConnectOptions>()
            .context("invalid URL logging database configuration")?;

        // read & parse ACLs ()
        config.mode_o_acl_rt = Some(ReAcl::new(&config.mode_o_acl)?);
        config.auto_o_acl_rt = Some(ReAcl::new(&config.auto_o_acl)?);
        config.invite_bl_userhost_rt = Some(ReAcl::new(&config.invite_bl_userhost)?);
        config.invite_bl_nick_rt = Some(ReAcl::new(&config.invite_bl_nick)?);

        // pre-compile url detection regex
        let url_re = Regex::new(&config.url_regex)?;
        if url_re.captures_len() < 2 {
            bail!("url_regex requires capture group 1");
        }
        config.url_re = Some(url_re);

        // prepare url-based commands, if any
        let mut tera = Tera::default();
        // Tera 2 removed its built-in `slugify` filter. Keep existing bot templates compatible.
        tera.register_filter("slugify", |value: &str, _: Kwargs, _: &State| slug::slugify(value));
        for (k, c) in config.url_cmd_list.iter_mut() {
            tera.add_raw_template(k, &c.url_tmpl)?;
            let filter = Regex::new(&c.output_filter)?;
            if filter.captures_len() < 2 {
                bail!("URL command {k} output_filter requires capture group 1");
            }
            c.output_filter_re = Some(filter);
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
    fn validate(&self) -> anyhow::Result<()> {
        for days in self.url_dup_expire_days.values() {
            if *days < 0
                || days
                    .checked_mul(86400)
                    .and_then(|seconds| Utc::now().timestamp().checked_sub(seconds))
                    .is_none()
            {
                bail!("URL expiry days must be nonnegative and fit in a timestamp");
            }
        }
        let mut commands = HashSet::new();
        for cmd in [
            &self.cmd_dumpacl,
            &self.cmd_invite,
            &self.cmd_join,
            &self.cmd_mode_o,
            &self.cmd_mode_v,
            &self.cmd_nick,
            &self.cmd_reload,
            &self.cmd_say,
            &self.cmd_version,
        ] {
            if cmd.is_empty() || cmd.chars().any(char::is_whitespace) || !commands.insert(cmd) {
                bail!("bot command names must be nonempty, unique single words");
            }
        }
        Ok(())
    }

    fn privileged(&self, nick: &str, userhost: &str, mapping: CaseMapping) -> anyhow::Result<bool> {
        if get_irc(&self.privileged_nicks, nick, mapping)? != Some(&true) {
            return Ok(false);
        }
        Ok(get_irc(&self.privileged_userhosts_rt, nick, mapping)?
            .is_none_or(|masks| masks.iter().any(|mask| mask.is_match(userhost))))
    }
}

fn read_config_file(path: &str) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("bot configuration exceeds 1 MiB");
    }
    Ok(bytes)
}

pub struct BotHandlers {
    handlers_irc_cmd: Vec<CmdHandler>,
    handlers_privmsg_open: HashMap<String, MsgHandler>,
    handlers_privmsg_priv: HashMap<String, MsgHandler>,
    handlers_chanmsg: HashMap<String, MsgHandler>,
}

pub struct BotState {
    pub my_nick: String,
    pub registered_since: Option<Instant>,
    pub msg_nick: String,
    pub msg_user: String,
    pub msg_host: String,
    pub msg_userhost: String,
    msg_memberships: HashMap<String, Membership>,

    op_sender: mpsc::Sender<Queued<OpEnvelope>>,
    msg_sender: mpsc::Sender<Queued<IrcOutput>>,
}

pub struct IrcBot {
    pub cli_opts: RwLock<OptsCommon>,
    pub config: RwLock<BotConfig>,
    pub state: RwLock<BotState>,
    pub handlers: RwLock<BotHandlers>,
    channel_modes: Arc<RwLock<ChannelModes>>,
    queues: Mutex<Option<WorkerQueues>>,
    http: HttpFetcher,
    discarded: AtomicUsize,
    expired: AtomicUsize,
}

impl IrcBot {
    pub async fn new(opts: &OptsCommon) -> anyhow::Result<(Self, irc::client::ClientStream)> {
        let mut bot_cfg = match BotConfig::new(&opts.bot_config) {
            Ok(b) => b,
            Err(e) => {
                bail!("{e}");
            }
        };
        bot_cfg.db = Some(start_db(&bot_cfg.url_log_db).await?);

        let irc_cfg = load_irc_config(&opts.irc_config)?;
        let irc = match timeout(Duration::from_secs(30), Client::from_config(irc_cfg))
            .await
            .context("IRC connection timed out")?
        {
            Ok(c) => c,
            Err(e) => {
                bail!("{e}");
            }
        };
        Self::with_client(opts, bot_cfg, irc)
    }

    fn with_client(
        opts: &OptsCommon,
        bot_cfg: BotConfig,
        mut irc: Client,
    ) -> anyhow::Result<(Self, irc::client::ClientStream)> {
        irc.identify()?;

        let my_nick = irc.current_nickname().to_string();
        let sender = irc.sender();
        let channel_modes = Arc::new(RwLock::new(ChannelModes::default()));
        let (op_sender, ops) = mpsc::channel(IRC_QUEUE_CAPACITY);
        let (msg_sender, output) = mpsc::channel(IRC_QUEUE_CAPACITY);
        let http = HttpFetcher::new()?;

        Ok((
            IrcBot {
                cli_opts: RwLock::new(opts.clone()),
                config: RwLock::new(bot_cfg),
                state: RwLock::new(BotState {
                    my_nick,
                    registered_since: None,
                    msg_nick: "NONE".into(),
                    msg_user: "NONE".into(),
                    msg_host: "NONE".into(),
                    msg_userhost: "NONE@NONE".into(),
                    msg_memberships: HashMap::new(),

                    op_sender,
                    msg_sender,
                }),
                handlers: RwLock::new(BotHandlers {
                    handlers_irc_cmd: Vec::with_capacity(INITIAL_HANDLERS),
                    handlers_privmsg_open: HashMap::with_capacity(INITIAL_HANDLERS),
                    handlers_privmsg_priv: HashMap::with_capacity(INITIAL_HANDLERS),
                    handlers_chanmsg: HashMap::with_capacity(INITIAL_HANDLERS),
                }),
                channel_modes,
                queues: Mutex::new(Some(WorkerQueues { ops, output, sender })),
                http,
                discarded: AtomicUsize::new(0),
                expired: AtomicUsize::new(0),
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
        let mut cfg = spawn_blocking(move || BotConfig::new(&config_file)).await??;
        cfg.db = Some(start_db(&cfg.url_log_db).await?);
        // The serial worker dispatches callbacks. Publish names and settings while holding both
        // locks; dispatch always releases the handler registry before invoking a callback.
        let mut current = self.config.write().await;
        let mut handlers = self.handlers.write().await;
        rename_handlers(
            &mut handlers.handlers_privmsg_open,
            &[
                (&current.cmd_invite, &cfg.cmd_invite),
                (&current.cmd_mode_o, &cfg.cmd_mode_o),
                (&current.cmd_mode_v, &cfg.cmd_mode_v),
                (&current.cmd_version, &cfg.cmd_version),
            ],
        );
        rename_handlers(
            &mut handlers.handlers_privmsg_priv,
            &[
                (&current.cmd_dumpacl, &cfg.cmd_dumpacl),
                (&current.cmd_join, &cfg.cmd_join),
                (&current.cmd_nick, &cfg.cmd_nick),
                (&current.cmd_reload, &cfg.cmd_reload),
                (&current.cmd_say, &cfg.cmd_say),
            ],
        );
        *current = cfg;
        info!("*** Reload successful.");
        Ok(true)
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

    pub async fn run(self: Arc<Self>, stream: irc::client::ClientStream) -> anyhow::Result<()> {
        let queues = self.queues.lock().await.take().context("IRC session already running")?;
        let (incoming_tx, incoming_rx) = mpsc::channel(IRC_QUEUE_CAPACITY);
        // Scoped futures: returning from either drops/cancels the other and its old queues.
        // No detached worker survives reconnect, and there is only one application worker.
        select! {
            result = self.clone().drive_irc(stream, queues.output, queues.sender, incoming_tx) => result,
            result = self.clone().work(incoming_rx, queues.ops) => result,
        }
    }

    async fn drive_irc(
        self: Arc<Self>,
        mut stream: irc::client::ClientStream,
        mut output: mpsc::Receiver<Queued<IrcOutput>>,
        sender: Sender,
        incoming: mpsc::Sender<Queued<Incoming>>,
    ) -> anyhow::Result<()> {
        let mut gate = OutputGate::default();
        let mut ready = true;
        let mut next_send = Instant::now();
        let registration_deadline = Instant::now() + IRC_REGISTRATION_TIMEOUT;
        let mut registered = false;
        let mut report = interval(Duration::from_secs(30));
        loop {
            let ack_deadline = gate
                .waiting
                .as_ref()
                .map_or(Instant::now() + Duration::from_secs(86400), |(_, deadline)| *deadline);
            select! {
                message = stream.next() => {
                    let Some(message) = message.transpose()? else { return Ok(()); };
                    trace!("Got msg: {message:?}");
                    if !matches!(message.prefix, Some(Prefix::Nickname(..))) {
                        gate.acknowledge(&message.command);
                    }
                    let msg_nick = message.source_nickname().unwrap_or("NONE").to_owned();
                    let mapping = self.channel_modes.read().await.case_mapping;
                    let my_nick = {
                        let mut state = self.state.write().await;
                        let old_nick = state.my_nick.clone();
                        if let Some(nick) = welcome_nickname(&message.command) {
                            registered = true;
                            state.registered_since.get_or_insert_with(Instant::now);
                            state.my_nick = nick.to_owned();
                        } else if let Command::NICK(nick) = &message.command && mapping.eq(&msg_nick, &old_nick) {
                            state.my_nick = nick.clone();
                        }
                        old_nick
                    };
                    self.track_channel_modes(&message.command, &msg_nick, &my_nick).await?;
                    let memberships = if matches!(message.command, Command::JOIN(..) | Command::PRIVMSG(..)) {
                        self.channel_modes.read().await.memberships(&msg_nick)
                    } else { HashMap::new() };
                    self.enqueue(&incoming, Incoming { message, my_nick, memberships })?;
                }
                item = output.recv(), if ready && registered && gate.waiting.is_none() => {
                    let item = item.context("IRC output queue closed")?;
                    let Some((_, item)) = self.take_fresh(item) else { continue; };
                    let (command, delay) = match item {
                        IrcOutput::Msg(msg) => {
                            info!("{} <{}> {}", msg.target, self.state.read().await.my_nick, msg.msg);
                            (Command::PRIVMSG(msg.target, msg.msg), Duration::from_millis(IRC_MSG_THROTTLE))
                        }
                        IrcOutput::Op(command, membership) => {
                            if let Some(member) = membership {
                                let modes = self.channel_modes.read().await;
                                if !modes.current(&member) || modes.has_oper(&member.channel, &member.nick) { continue; }
                            }
                            (command, Duration::from_millis(IRC_OP_THROTTLE))
                        }
                    };
                    sender.send(command)?;
                    gate.sent += 1;
                    if gate.sent == OUTPUT_BATCH {
                        let token = format!("sjmb-{:016x}", rand::random::<u64>());
                        sender.send(Command::PING(token.clone(), None))?;
                        gate.waiting = Some((token, Instant::now() + OUTPUT_ACK_TIMEOUT));
                    }
                    ready = false;
                    next_send = Instant::now() + delay;
                }
                _ = sleep_until(next_send), if !ready => ready = true,
                _ = sleep_until(ack_deadline), if gate.waiting.is_some() => bail!("IRC output checkpoint timed out"),
                _ = sleep_until(registration_deadline), if !registered => bail!("IRC registration timed out"),
                _ = report.tick() => {
                    let count = self.discarded.swap(0, AtomicOrdering::Relaxed);
                    if count != 0 { error!("Discarded {count} requests or replies because a bounded queue was full"); }
                    let count = self.expired.swap(0, AtomicOrdering::Relaxed);
                    if count != 0 { error!("Discarded {count} requests or replies because their queue deadline expired"); }
                }
            }
        }
    }

    async fn work(
        self: Arc<Self>,
        mut incoming: mpsc::Receiver<Queued<Incoming>>,
        mut ops: mpsc::Receiver<Queued<OpEnvelope>>,
    ) -> anyhow::Result<()> {
        let mut op_ready = true;
        let mut next_op = Instant::now();
        loop {
            select! {
                event = incoming.recv() => {
                    let event = event.context("IRC input queue closed")?;
                    let Some((deadline, event)) = self.take_fresh(event) else { continue; };
                    if let Err(error) = REQUEST_DEADLINE.scope(deadline, self.clone().handle_message(event)).await { error!("IRC handler failed: {error:#}"); }
                }
                job = ops.recv(), if op_ready => {
                    let job = job.context("IRC operation queue closed")?;
                    let Some((deadline, job)) = self.take_fresh(job) else { continue; };
                    if let Some(member) = &job.membership {
                        let modes = self.channel_modes.read().await;
                        if !modes.current(member) || modes.has_oper(&member.channel, &member.nick) { continue; }
                    }
                    if let Err(error) = REQUEST_DEADLINE.scope(deadline, op_dispatch(self.clone(), job)).await { error!("IRC operation failed: {error:#}"); }
                    op_ready = false;
                    next_op = Instant::now() + Duration::from_millis(IRC_OP_THROTTLE + rand::random_range(0..=IRC_OP_THROTTLE_JITTER));
                }
                _ = sleep_until(next_op), if !op_ready => op_ready = true,
            }
        }
    }

    async fn handle_message(self: Arc<Self>, event: Incoming) -> anyhow::Result<()> {
        let Incoming {
            message,
            my_nick,
            memberships,
        } = event;
        {
            let mut state = self.state.write().await;
            state.msg_memberships = memberships;
            if let Some(Prefix::Nickname(nick, user, host)) = &message.prefix {
                state.msg_nick = nick.clone();
                state.msg_user = user.clone();
                state.msg_host = host.clone();
                state.msg_userhost = format!("{user}@{host}");
            } else {
                state.msg_nick = "NONE".into();
                state.msg_user = "NONE".into();
                state.msg_host = "NONE".into();
                state.msg_userhost = "NONE@NONE".into();
            }
        }
        let handlers = self.handlers.read().await.handlers_irc_cmd.clone();
        for handler in handlers {
            match handler(self.clone(), message.command.clone()).await {
                Ok(true) => break,
                Err(error) => error!("IRC callback failed: {error:#}"),
                _ => {}
            }
        }
        if let Command::PRIVMSG(channel, msg) = message.command {
            let (cmd, args) = msg.split_once(char::is_whitespace).unwrap_or((msg.as_str(), ""));
            let (cmd, args) = (cmd.to_owned(), args.to_owned());
            if self.same_nick(&channel, &my_nick).await {
                self.handle_privmsg(msg, cmd, args).await?;
            } else {
                self.handle_chanmsg(channel, msg, cmd, args).await?;
            }
        }
        Ok(())
    }

    fn take_fresh<T>(&self, queued: Queued<T>) -> Option<(Instant, T)> {
        if Instant::now() >= queued.expires_at {
            self.expired.fetch_add(1, AtomicOrdering::Relaxed);
            None
        } else {
            Some((queued.expires_at, queued.item))
        }
    }

    fn enqueue<T>(&self, sender: &mpsc::Sender<Queued<T>>, item: T) -> anyhow::Result<bool> {
        let expires_at = REQUEST_DEADLINE
            .try_with(|deadline| *deadline)
            .unwrap_or_else(|_| Instant::now() + MAX_QUEUE_AGE);
        if Instant::now() >= expires_at {
            self.expired.fetch_add(1, AtomicOrdering::Relaxed);
            return Ok(false);
        }
        match sender.try_send(Queued { expires_at, item }) {
            Ok(()) => Ok(true),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.discarded.fetch_add(1, AtomicOrdering::Relaxed);
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => bail!("IRC worker queue closed"),
        }
    }

    async fn track_channel_modes(&self, cmd: &Command, msg_nick: &str, my_nick: &str) -> anyhow::Result<()> {
        let mut channel_modes = self.channel_modes.write().await;
        match cmd {
            Command::JOIN(channel, _, _) => channel_modes.join(channel, msg_nick),
            Command::PART(channel, _) => channel_modes.part(channel, msg_nick, my_nick),
            Command::KICK(channel, nick, _) => channel_modes.kick(channel, nick, my_nick),
            Command::QUIT(_) => channel_modes.quit(msg_nick),
            Command::NICK(new_nick) => channel_modes.nick_change(msg_nick, new_nick),
            Command::ChannelMODE(channel, modes) => channel_modes.apply_modes(channel, modes),
            Command::Response(Response::RPL_NAMREPLY, args) => channel_modes.apply_namreply(args),
            Command::Response(Response::RPL_ISUPPORT, args) => {
                for value in args.iter().skip(1) {
                    if let Some(value) = value.strip_prefix("CASEMAPPING=") {
                        channel_modes.set_case_mapping(CaseMapping::parse(value)?)?;
                    } else if value == "-CASEMAPPING" {
                        channel_modes.set_case_mapping(CaseMapping::default())?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn same_nick(&self, left: &str, right: &str) -> bool {
        self.channel_modes.read().await.case_mapping.eq(left, right)
    }

    pub async fn new_op(self: Arc<Self>, op: IrcOp) -> anyhow::Result<bool> {
        let mapping = self.channel_modes.read().await.case_mapping;
        let membership = if let IrcOp::ModeOper(channel, nick) = &op {
            let Some(member) = self
                .state
                .read()
                .await
                .msg_memberships
                .get(&mapping.fold(channel))
                .filter(|member| mapping.eq(&member.nick, nick))
                .cloned()
            else {
                return Ok(false);
            };
            Some(member)
        } else {
            None
        };
        let sender = self.state.read().await.op_sender.clone();
        self.enqueue(&sender, OpEnvelope { op, membership })
    }

    pub async fn new_msg(self: Arc<Self>, target: &str, msg: &str) -> anyhow::Result<bool> {
        valid_irc_atom(target)?;
        let msg = irc_text(msg, 400.min(510usize.saturating_sub(10 + target.len())));
        if msg.is_empty() {
            return Ok(false);
        }
        let sender = self.state.read().await.msg_sender.clone();
        self.enqueue(
            &sender,
            IrcOutput::Msg(IrcMsg {
                target: target.into(),
                msg,
            }),
        )
    }

    async fn output_op(&self, command: Command, membership: Option<Membership>) -> anyhow::Result<bool> {
        let sender = self.state.read().await.msg_sender.clone();
        self.enqueue(&sender, IrcOutput::Op(command, membership))
    }

    // Process private messages here and return true only if something was reacted upon
    async fn handle_privmsg(self: Arc<Self>, msg: String, cmd: String, args: String) -> anyhow::Result<bool> {
        let nick = self.state.read().await.msg_nick.clone();
        let userhost = &self.state.read().await.msg_userhost.clone();
        info!("*** Privmsg from {nick} ({userhost}): {cmd} {args}");

        let mapping = self.channel_modes.read().await.case_mapping;
        let nick_is_privileged = self.config.read().await.privileged(&nick, userhost, mapping)?;

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
        let handler = self.handlers.read().await.handlers_privmsg_priv.get(&cmd).cloned();
        match handler {
            Some(handler) => handler(self.clone(), msg, cmd, args).await,
            _ => Ok(false), // did not recognize any command
        }
    }

    // Process "public" commands here and return true only if something was reacted upon
    async fn handle_privmsg_open(self: Arc<Self>, msg: String, cmd: String, args: String) -> anyhow::Result<bool> {
        let handler = self.handlers.read().await.handlers_privmsg_open.get(&cmd).cloned();
        match handler {
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

        let handler = self.handlers.read().await.handlers_chanmsg.get(&cmd).cloned();
        if let Some(handler) = handler {
            return handler(self.clone(), msg, cmd, args).await;
        }

        let mapping = self.channel_modes.read().await.case_mapping;
        let (handled, queued_actions) = {
            let cfg = self.config.read().await;
            let mut queued_actions = Vec::new();

            // url_cmd starts with '!'
            if let (Some(u_cmd), Some(true)) = (
                cmd.strip_prefix('!'),
                get_irc_wild(&cfg.url_cmd_channels, &channel, mapping)?,
            ) && let Some(c) = cfg.url_cmd_list.get(u_cmd)
            {
                let u_args = args.split_whitespace().collect::<Vec<&str>>();
                debug!("Url cmd ctx arg: {args:?}");
                debug!("Url cmd ctx args: {u_args:?}");

                let mut ctx = TeraContext::new();
                ctx.insert("arg", &args);
                ctx.insert("args", &u_args);
                debug!("Url cmd ctx: {ctx:#?}");

                let url = cfg
                    .url_cmd_tera
                    .as_ref()
                    .ok_or_else(|| anyhow!("No tera"))?
                    .render(u_cmd, &ctx)?;
                if url.len() > MAX_URL_BYTES {
                    bail!("rendered URL is too long");
                }
                info!("URL cmd: !{u_cmd} --> {url}");
                let f = c
                    .output_filter_re
                    .as_ref()
                    .ok_or_else(|| anyhow!("No output regex"))?
                    .clone();
                queued_actions.push(QueuedAction::Op(IrcOp::UrlFetch(url, channel.to_string(), f)));
                (true, queued_actions)
            } else {
                let mut found_url = false;
                'outer: for url_cap in cfg
                    .url_re
                    .as_ref()
                    .ok_or_else(|| anyhow!("No url_regex_re"))?
                    .captures_iter(msg.as_ref())
                    .take(MAX_URLS_PER_MESSAGE)
                {
                    found_url = true;
                    let Some(url) = url_cap.get(1) else {
                        continue;
                    };
                    if url.as_str().len() > MAX_URL_BYTES {
                        continue;
                    }
                    let url_s = url.as_str().to_owned();
                    info!("*** ({nick} at {channel}) detected url: {url_s}");

                    for b in &cfg.url_blacklist {
                        if url_s.starts_with(b) {
                            info!("*** Blacklilsted URL. Ignored.");
                            continue 'outer;
                        }
                    }

                    if let Some(true) = get_irc_wild(&cfg.url_log_channels, &channel, mapping)? {
                        let db = cfg
                            .db
                            .clone()
                            .ok_or_else(|| anyhow!("No database pool for URL logging"))?;

                        if let Some(true) = get_irc_wild(&cfg.url_dup_complain_channels, &channel, mapping)? {
                            let expire_days = get_irc_wild(&cfg.url_dup_expire_days, &channel, mapping)?.unwrap_or(&7);
                            let tz = get_irc_wild(
                                cfg.url_dup_tz.as_ref().context("missing parsed timezones")?,
                                &channel,
                                mapping,
                            )?
                            .unwrap_or(&Tz::UTC);
                            queued_actions.push(QueuedAction::Op(IrcOp::UrlCheck(
                                db.clone(),
                                url_s.clone(),
                                channel.to_owned(),
                                tz.to_owned(),
                                expire_days.to_owned(),
                            )));
                        }

                        let op = IrcOp::UrlLog(
                            db,
                            url_s.clone(),
                            channel.to_owned(),
                            nick.to_owned(),
                            Utc::now().timestamp(),
                        );
                        debug!("New op: {op:?}");
                        queued_actions.push(QueuedAction::Op(op));
                    }

                    if let Some(true) = get_irc_wild(&cfg.url_fetch_channels, &channel, mapping)? {
                        queued_actions.push(QueuedAction::Op(IrcOp::UrlTitle(url_s.clone(), channel.to_owned())));
                    }

                    if let Some(true) = get_irc_wild(&cfg.url_mut_channels, &channel, mapping)?
                        && let Some((_i, new_url)) = cfg
                            .url_mut_re
                            .as_ref()
                            .ok_or_else(|| anyhow!("No url_mut_re"))?
                            .re_mut(&url_s)
                    {
                        if new_url.len() > MAX_URL_BYTES {
                            continue;
                        }
                        debug!("Doing url mut");
                        queued_actions.push(QueuedAction::Msg(IrcMsg {
                            target: channel.clone(),
                            msg: new_url.clone(),
                        }));
                        queued_actions.push(QueuedAction::Op(IrcOp::UrlTitle(new_url, channel.to_string())));
                    }
                }

                (found_url, queued_actions)
            }
        };

        for queued_action in queued_actions {
            match queued_action {
                QueuedAction::Msg(queued_msg) => {
                    self.clone().new_msg(&queued_msg.target, &queued_msg.msg).await?;
                }
                QueuedAction::Op(queued_op) => {
                    self.clone().new_op(queued_op).await?;
                }
            }
        }

        // more processing might happen here
        if handled {
            return Ok(true);
        }
        // ...or here

        Ok(false)
    }
}

fn rename_handlers(map: &mut HashMap<String, MsgHandler>, names: &[(&String, &String)]) {
    let renamed: Vec<_> = names
        .iter()
        .filter_map(|(old, new)| map.remove(*old).map(|handler| ((*new).clone(), handler)))
        .collect();
    map.extend(renamed);
}

fn valid_irc_atom(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with(':')
        || value.contains(',')
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("invalid IRC target or argument");
    }
    Ok(())
}

async fn op_dispatch(bot: Arc<IrcBot>, job: OpEnvelope) -> anyhow::Result<()> {
    let OpEnvelope { op, membership } = job;
    let command = match op {
        IrcOp::Invite(nick, channel) => {
            valid_irc_atom(&nick)?;
            valid_irc_atom(&channel)?;
            Some(Command::INVITE(nick, channel))
        }
        IrcOp::Join(channel) => {
            valid_irc_atom(&channel)?;
            Some(Command::JOIN(channel, None, None))
        }
        IrcOp::ModeOper(channel, nick) => {
            valid_irc_atom(&channel)?;
            valid_irc_atom(&nick)?;
            Some(Command::ChannelMODE(
                channel,
                vec![Mode::Plus(ChannelMode::Oper, Some(nick))],
            ))
        }
        IrcOp::ModeVoice(channel, nick) => {
            valid_irc_atom(&channel)?;
            valid_irc_atom(&nick)?;
            Some(Command::ChannelMODE(
                channel,
                vec![Mode::Plus(ChannelMode::Voice, Some(nick))],
            ))
        }
        IrcOp::Nick(nick) => {
            valid_irc_atom(&nick)?;
            Some(Command::NICK(nick))
        }
        IrcOp::UrlCheck(db, url, channel, tz, days) => {
            op_handle_urlcheck(bot.clone(), db, url, channel, tz, days).await?;
            None
        }
        IrcOp::UrlFetch(url, channel, filter) => {
            op_handle_urlfetch(bot.clone(), url, channel, filter).await?;
            None
        }
        IrcOp::UrlLog(db, url, chan, nick, ts) => {
            db_add_url(&db, &UrlCtx { ts, chan, nick, url }).await?;
            None
        }
        IrcOp::UrlTitle(url, channel) => {
            op_handle_urltitle(bot.clone(), url, channel).await?;
            None
        }
        IrcOp::Reload(nick) => {
            match bot.reload().await {
                Ok(_) => {
                    bot.clone().new_msg(&nick, "*** Reload successful.").await?;
                }
                Err(error) => {
                    bot.clone()
                        .new_msg(&nick, &format!("*** Reload failed: {error}"))
                        .await?;
                    return Err(error);
                }
            }
            None
        }
    };
    if let Some(command) = command {
        bot.output_op(command, membership).await?;
    }
    Ok(())
}

async fn op_handle_urlcheck(
    bot: Arc<IrcBot>,
    db: DbCtx,
    url: String,
    channel: String,
    tz: Tz,
    days: i64,
) -> anyhow::Result<()> {
    let seconds = days
        .checked_mul(86400)
        .filter(|value| *value >= 0)
        .context("invalid URL expiry days")?;
    if let Some(old) = db_check_url(&db, &url, &channel, seconds).await?
        && let (Some(first), Some(last)) = (old.first, old.last)
    {
        let first = duplicate_timestamp(first, tz)?;
        let text = match old.cnt.cmp(&1) {
            Ordering::Equal => Some(format!("Wanha URL, nähty {first}")),
            Ordering::Greater => {
                let last = duplicate_timestamp(last, tz)?;
                Some(format!(
                    "Wanha URL, nähty {} kertaa, ensin {first} ja viimeksi {last}",
                    old.cnt
                ))
            }
            _ => None,
        };
        if let Some(text) = text {
            bot.new_msg(&channel, &text).await?;
        }
    }
    Ok(())
}

fn duplicate_timestamp(timestamp: i64, tz: Tz) -> anyhow::Result<String> {
    let utc = DateTime::from_timestamp(timestamp, 0).context("invalid URL timestamp")?;
    let local = utc.with_timezone(&tz);
    // DateTime's Display can panic if the offset pushes a boundary timestamp out of range.
    let naive = utc
        .naive_utc()
        .checked_add_offset(local.offset().fix())
        .context("URL timestamp outside local timezone range")?;
    Ok(format!("{naive} {}", local.offset()))
}

async fn op_handle_urlfetch(
    bot: Arc<IrcBot>,
    url: String,
    channel: String,
    output_filter: Regex,
) -> anyhow::Result<()> {
    if let Some(text) = bot.http.get_text(&url).await? {
        for capture in output_filter.captures_iter(&text.body).take(MAX_FETCH_REPLIES) {
            if let Some(value) = capture.get(1) {
                // Truncation may cut the final record. Earlier complete matches remain useful.
                if text.truncated && value.end() == text.body.len() {
                    continue;
                }
                bot.clone()
                    .new_msg(&channel, &format!("--> {}", irc_text(value.as_str(), 396)))
                    .await?;
            }
        }
    }
    Ok(())
}

async fn op_handle_urltitle(bot: Arc<IrcBot>, url: String, channel: String) -> anyhow::Result<()> {
    if let Some(text) = bot.http.get_text(&url).await?
        && let Some(title) = extract_title(&text.body)
        && title != url
    {
        bot.new_msg(&channel, &format!("\"{title}\"")).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestConfig(PathBuf);

    impl TestConfig {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("sjmb-test-{}-{}.json", process::id(), rand::random::<u64>()));
            fs::write(&path, include_str!("../config/sjmb.json")).unwrap();
            Self(path)
        }

        fn edit(&self, edit: impl FnOnce(&mut serde_json::Value)) {
            let mut value = serde_json::from_slice(&fs::read(&self.0).unwrap()).unwrap();
            edit(&mut value);
            fs::write(&self.0, serde_json::to_vec(&value).unwrap()).unwrap();
        }

        fn load(&self) -> anyhow::Result<BotConfig> {
            BotConfig::new(self.0.to_str().unwrap())
        }

        async fn bot(&self, irc_config: Config) -> (Arc<IrcBot>, irc::client::ClientStream) {
            let opts = OptsCommon {
                verbose: false,
                debug: false,
                trace: false,
                bot_config: self.0.to_string_lossy().into_owned(),
                irc_config: String::new(),
            };
            let client = Client::from_config(irc_config).await.unwrap();
            let mut config = self.load().unwrap();
            config.db = Some(start_db(&config.url_log_db).await.unwrap());
            let (bot, stream) = IrcBot::with_client(&opts, config, client).unwrap();
            (Arc::new(bot), stream)
        }

        async fn mock_bot(&self) -> Arc<IrcBot> {
            self.bot(Config {
                nickname: Some("testbot".into()),
                use_mock_connection: true,
                ..Default::default()
            })
            .await
            .0
        }
    }

    impl Drop for TestConfig {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[tokio::test]
    async fn full_queue_discards_without_waiting_and_closed_queue_errors() {
        let config = TestConfig::new();
        let bot = config.mock_bot().await;
        for _ in 0..IRC_QUEUE_CAPACITY {
            assert!(bot.clone().new_msg("#test", "message").await.unwrap());
        }
        assert!(!bot.clone().new_msg("#test", "discarded").await.unwrap());
        assert_eq!(bot.discarded.load(AtomicOrdering::Relaxed), 1);
        drop(bot.queues.lock().await.take());
        assert!(bot.clone().new_msg("#test", "closed").await.is_err());
    }

    #[tokio::test]
    async fn queue_hops_preserve_deadlines_and_expired_work_is_discarded() {
        let config = TestConfig::new();
        let bot = config.mock_bot().await;
        let deadline = Instant::now() + Duration::from_secs(60);
        let (tx, mut rx) = mpsc::channel(1);
        REQUEST_DEADLINE
            .scope(deadline, async {
                assert!(bot.enqueue(&tx, "request").unwrap());
            })
            .await;
        let (original, request) = bot.take_fresh(rx.try_recv().unwrap()).unwrap();
        assert_eq!(original, deadline);
        REQUEST_DEADLINE
            .scope(original, async {
                assert!(bot.clone().new_msg("#test", request).await.unwrap());
            })
            .await;
        let output = bot.queues.lock().await.as_mut().unwrap().output.try_recv().unwrap();
        assert_eq!(output.expires_at, deadline);
        // A reply expiring while queued must be discarded at consumption too.
        assert!(
            bot.take_fresh(Queued {
                expires_at: Instant::now(),
                item: output.item
            })
            .is_none()
        );
        REQUEST_DEADLINE
            .scope(Instant::now(), async {
                assert!(!bot.clone().new_msg("#test", "too late").await.unwrap());
            })
            .await;
        assert_eq!(bot.expired.load(AtomicOrdering::Relaxed), 2);
        assert!(bot.queues.lock().await.as_mut().unwrap().output.try_recv().is_err());
    }

    #[tokio::test]
    async fn callback_can_modify_registry_without_deadlocking() {
        let config = TestConfig::new();
        let bot = config.mock_bot().await;
        bot.register_privmsg_open(
            "clear",
            into_msg_handler(|bot, _, _, _| async move {
                bot.clear_handlers().await;
                Ok(true)
            }),
        )
        .await;
        let result = timeout(
            Duration::from_secs(1),
            bot.handle_privmsg_open(String::new(), "clear".into(), String::new()),
        )
        .await;
        assert!(result.unwrap().unwrap());
    }

    #[tokio::test]
    async fn reload_updates_command_names_and_preserves_working_settings_on_error() {
        let config = TestConfig::new();
        let bot = config.mock_bot().await;
        bot.register_privmsg_open("version", into_msg_handler(|_, _, _, _| async { Ok(true) }))
            .await;
        config.edit(|json| json["cmd_version"] = "v2".into());
        bot.reload().await.unwrap();
        assert_eq!(bot.config.read().await.cmd_version, "v2");
        assert!(bot.handlers.read().await.handlers_privmsg_open.contains_key("v2"));
        assert!(!bot.handlers.read().await.handlers_privmsg_open.contains_key("version"));
        fs::write(&config.0, "{}").unwrap();
        assert!(bot.reload().await.is_err());
        assert_eq!(bot.config.read().await.cmd_version, "v2");
        assert!(bot.handlers.read().await.handlers_privmsg_open.contains_key("v2"));
    }

    #[test]
    fn validates_captures_expiry_and_optional_userhost_masks() {
        let config = TestConfig::new();
        config.edit(|json| json["privileged_userhosts"] = serde_json::json!({"sjm": ["~?ser@*.example.com"]}));
        let loaded = config.load().unwrap();
        assert!(
            loaded
                .privileged("sjm", "~user@host.example.com", CaseMapping::default())
                .unwrap()
        );
        assert!(
            !loaded
                .privileged("sjm", "~user@host.example.com.evil", CaseMapping::default())
                .unwrap()
        );
        assert!(
            !loaded
                .privileged("other", "~user@host.example.com", CaseMapping::default())
                .unwrap()
        );
        config.edit(|json| json["url_dup_expire_days"]["*"] = (-1).into());
        assert!(config.load().is_err());
        config.edit(|json| json["url_dup_expire_days"]["*"] = i64::MAX.into());
        assert!(config.load().is_err());
        config.edit(|json| {
            json["url_dup_expire_days"]["*"] = 7.into();
            json["url_regex"] = "https?://.+".into();
        });
        assert!(config.load().is_err());
    }

    #[tokio::test]
    async fn unmatched_optional_capture_is_skipped() {
        let config = TestConfig::new();
        config.edit(|json| json["url_regex"] = "(https)?www".into());
        let bot = config.mock_bot().await;
        assert!(
            bot.handle_chanmsg("#chana".into(), "www".into(), "www".into(), String::new())
                .await
                .unwrap()
        );
    }

    #[test]
    fn pending_grants_do_not_survive_nickname_reuse() {
        let mut modes = ChannelModes::default();
        modes.join("#test", "alice");
        let old = modes.membership("#test", "alice").unwrap();
        modes.quit("alice");
        modes.join("#test", "alice");
        assert!(!modes.current(&old));
    }

    #[test]
    fn names_and_mode_entries_cannot_reuse_old_grant_identities() {
        let names = ["bot".into(), "=".into(), "#test".into(), "alice bot".into()];
        let mut modes = ChannelModes::default();
        modes.apply_namreply(&names);
        let old = modes.membership("#test", "alice").unwrap();
        // Repeated NAMES for the same live membership should keep its identity.
        modes.apply_namreply(&names);
        assert!(modes.current(&old));
        modes.kick("#test", "BOT", "bot");
        modes.join("#test", "bot");
        modes.apply_namreply(&names);
        assert!(!modes.current(&old));

        modes.quit("alice");
        let mode = [Mode::Plus(ChannelMode::Voice, Some("alice".into()))];
        modes.apply_modes("#test", &mode);
        let old = modes.membership("#test", "alice").unwrap();
        modes.part("#test", "alice", "bot");
        modes.apply_modes("#test", &mode);
        assert!(!modes.current(&old));
    }

    #[test]
    fn channel_tracking_uses_server_case_mapping_and_invalidates_nick_changes() {
        let mut modes = ChannelModes::default();
        modes.set_case_mapping(CaseMapping::StrictRfc1459).unwrap();
        modes.join("#Test[", "Alice[");
        let old = modes.membership("#TEST{", "ALICE{").unwrap();
        modes.apply_modes("#TEST{", &[Mode::Plus(ChannelMode::Oper, Some("ALICE{".into()))]);
        assert!(modes.has_oper("#test[", "alice["));
        modes.nick_change("ALICE[", "alice[");
        assert!(!modes.current(&old));
        assert!(modes.has_oper("#test{", "alice{"));
        assert!(modes.set_case_mapping(CaseMapping::Ascii).is_err());
        modes.part("#TEST{", "ALICE{", "bot");
        assert!(modes.membership("#test[", "alice[").is_none());
    }

    #[tokio::test]
    async fn isupport_controls_case_mapping_and_private_message_routing() {
        let config = TestConfig::new();
        let bot = config.mock_bot().await;
        let isupport: Message = ":server 005 testbot CASEMAPPING=ascii :supported".parse().unwrap();
        bot.track_channel_modes(&isupport.command, "NONE", "testbot")
            .await
            .unwrap();
        assert_eq!(bot.channel_modes.read().await.case_mapping, CaseMapping::Ascii);
        bot.register_privmsg_open(
            "check",
            into_msg_handler(|bot, _, _, _| async move {
                bot.clone().new_msg("user", "reached private handler").await?;
                Ok(true)
            }),
        )
        .await;
        bot.clone()
            .handle_message(Incoming {
                message: ":user!u@h PRIVMSG TESTBOT :check".parse().unwrap(),
                my_nick: "testbot".into(),
                memberships: HashMap::new(),
            })
            .await
            .unwrap();
        assert!(bot.queues.lock().await.as_mut().unwrap().output.try_recv().is_ok());
    }

    #[test]
    fn nick_case_cannot_bypass_userhost_restrictions() {
        let config = TestConfig::new();
        config.edit(|json| json["privileged_userhosts"] = serde_json::json!({"SJM": ["user@host"]}));
        let cfg = config.load().unwrap();
        assert!(cfg.privileged("SjM", "user@host", CaseMapping::Ascii).unwrap());
        assert!(!cfg.privileged("SjM", "user@elsewhere", CaseMapping::Ascii).unwrap());
        assert!(!cfg.privileged("SjM", "USER@HOST", CaseMapping::Ascii).unwrap());
        config.edit(|json| json["privileged_nicks"]["SJM"] = false.into());
        assert!(
            config
                .load()
                .unwrap()
                .privileged("sjm", "user@host", CaseMapping::Ascii)
                .is_err()
        );
    }

    #[test]
    fn rejects_database_timestamps_outside_local_timezone_range() {
        assert!(duplicate_timestamp(i64::MAX, Tz::UTC).is_err());
        assert!(duplicate_timestamp(DateTime::<Utc>::MAX_UTC.timestamp(), "Etc/GMT-14".parse().unwrap()).is_err());
    }

    #[test]
    fn output_checkpoint_requires_matching_pong() {
        let mut gate = OutputGate {
            sent: OUTPUT_BATCH,
            waiting: Some(("checkpoint".into(), Instant::now())),
        };
        gate.acknowledge(&Command::PONG("server".into(), Some("unrelated".into())));
        assert!(gate.waiting.is_some());
        gate.acknowledge(&Command::PONG("server".into(), Some("checkpoint".into())));
        assert!(gate.waiting.is_none());
        assert_eq!(gate.sent, 0);
    }

    #[tokio::test]
    async fn socket_answers_keepalive_and_ctcp_during_slow_handler_and_disconnect_cancels_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = TestConfig::new();
        let (bot, stream) = config
            .bot(Config {
                nickname: Some("testbot".into()),
                server: Some("127.0.0.1".into()),
                port: Some(port),
                use_tls: Some(false),
                version: Some("sjmb-test".into()),
                ..Default::default()
            })
            .await;
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        struct OnDrop(Arc<AtomicBool>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.store(true, AtomicOrdering::Relaxed);
            }
        }
        let (notification, flag) = (started.clone(), cancelled.clone());
        bot.register_privmsg_open(
            "slow",
            into_msg_handler(move |_, _, _, _| {
                let (notification, flag) = (notification.clone(), flag.clone());
                async move {
                    let _on_drop = OnDrop(flag);
                    notification.notify_one();
                    pending::<()>().await;
                    Ok(true)
                }
            }),
        )
        .await;
        let run = spawn(bot.run(stream));
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        socket
            .get_mut()
            .write_all(
                b":server 001 testbot :Welcome\r\n:server 376 testbot :End\r\n:user!u@h PRIVMSG testbot :slow\r\n",
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(2), started.notified()).await.unwrap();
        socket
            .get_mut()
            .write_all(b"PING :while-busy\r\n:user!u@h PRIVMSG testbot :\x01VERSION\x01\r\n")
            .await
            .unwrap();
        timeout(Duration::from_secs(2), async {
            let mut line = String::new();
            let (mut pong, mut ctcp) = (false, false);
            while !pong || !ctcp {
                line.clear();
                assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
                pong |= line.contains("PONG while-busy");
                ctcp |= line.contains("NOTICE user :\x01VERSION sjmb-test\x01");
            }
        })
        .await
        .unwrap();
        drop(socket);
        timeout(Duration::from_secs(2), run).await.unwrap().unwrap().unwrap();
        assert!(cancelled.load(AtomicOrdering::Relaxed));
    }

    #[test]
    fn welcome_response_reports_server_accepted_nickname() {
        let message: Message = ":irc.example 001 sjmbot_ :Welcome to IRC"
            .parse()
            .expect("welcome should parse");
        assert_eq!(welcome_nickname(&message.command), Some("sjmbot_"));

        let message: Message = ":irc.example 002 sjmbot_ :Your host is irc.example"
            .parse()
            .expect("server info should parse");
        assert_eq!(welcome_nickname(&message.command), None);
    }

    #[test]
    fn namreply_tracks_operator_prefix() {
        let mut modes = ChannelModes::default();
        modes.apply_namreply(&[
            "sjmb".to_string(),
            "=".to_string(),
            "#test".to_string(),
            "@alice +bob charlie".to_string(),
        ]);

        assert!(modes.has_oper("#test", "alice"));
        assert!(!modes.has_oper("#test", "bob"));
        assert!(!modes.has_oper("#test", "charlie"));
    }

    #[test]
    fn mode_changes_update_operator_state() {
        let mut modes = ChannelModes::default();
        modes.join("#test", "alice");
        modes.apply_modes("#test", &[Mode::Plus(ChannelMode::Oper, Some("alice".to_string()))]);
        assert!(modes.has_oper("#test", "alice"));

        modes.apply_modes("#test", &[Mode::Minus(ChannelMode::Oper, Some("alice".to_string()))]);
        assert!(!modes.has_oper("#test", "alice"));
    }

    #[test]
    fn missing_version_command_uses_default() {
        let mut config_json: serde_json::Value =
            serde_json::from_str(include_str!("../config/sjmb.json")).expect("example bot config should be valid JSON");
        config_json
            .as_object_mut()
            .expect("example bot config should be an object")
            .remove("cmd_version");

        let config: BotConfig = serde_json::from_value(config_json).expect("legacy bot config should deserialize");
        assert_eq!(config.cmd_version, "version");
    }

    #[test]
    fn example_url_templates_render_with_tera_2() {
        let config_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config/sjmb.json");
        let config = BotConfig::new(config_path).expect("example bot config should load");
        let tera = config
            .url_cmd_tera
            .expect("URL command templates should be initialized");
        let args = vec!["Hélsinki!"];
        let mut context = TeraContext::new();
        context.insert("arg", "Hélsinki!");
        context.insert("args", &args);

        let url = tera
            .render("metar_old", &context)
            .expect("example URL template should render");
        assert_eq!(
            url,
            "https://tgftp.nws.noaa.gov/data/observations/metar/stations/HELSINKI.TXT"
        );
    }
}
// EOF
