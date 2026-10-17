// bin/sjmb.rs

#![forbid(unsafe_code)]

use sjmb::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut opts = OptsCommon::parse();
    opts.finalize()?;
    opts.start_pgm(env!("CARGO_BIN_NAME"))?;

    // Fail clearly for invalid startup configuration; network failures are retried below.
    BotConfig::new(&opts.bot_config)?;
    load_irc_config(&opts.irc_config)?;
    let mut backoff = 2u64;
    loop {
        select! {
            result = run_session(&opts, &mut backoff) => {
                if let Err(error) = result { error!("IRC session ended: {error:#}"); }
            }
            result = ctrl_c() => { result?; return Ok(()); }
        }
        let delay = Duration::from_millis(backoff * 1000 + rand::random_range(0..=1000));
        warn!("Reconnecting in {} seconds", delay.as_secs_f32());
        select! {
            _ = sleep(delay) => {},
            result = ctrl_c() => { result?; return Ok(()); }
        }
        backoff = (backoff * 2).min(30);
    }
}

async fn run_session(opts: &OptsCommon, backoff: &mut u64) -> anyhow::Result<()> {
    let (bot, stream) = IrcBot::new(opts).await?;
    let bot = Arc::new(bot);
    bot_cmd_setup(bot.clone()).await?;
    let result = bot.clone().run(stream).await;
    if bot
        .state
        .read()
        .await
        .registered_since
        .is_some_and(|started| started.elapsed() >= Duration::from_secs(60))
    {
        *backoff = 2;
    }
    result
}

async fn bot_cmd_setup(bot: Arc<IrcBot>) -> anyhow::Result<()> {
    bot.clear_handlers().await;

    // Register JOIN callback
    bot.register_irc_cmd_for(
        |command| matches!(command, Command::JOIN(..)),
        into_cmd_handler(handle_join),
    )
    .await;

    // ### Register commands
    let config = bot.config.read().await;

    // these can be used by anyone (open)
    bot.register_privmsg_open(&config.cmd_invite, into_msg_handler(handle_open_cmd_invite))
        .await;
    bot.register_privmsg_open(&config.cmd_mode_o, into_msg_handler(handle_open_cmd_mode_o))
        .await;
    bot.register_privmsg_open(&config.cmd_mode_v, into_msg_handler(handle_open_cmd_mode_v))
        .await;
    bot.register_privmsg_open(&config.cmd_version, into_msg_handler(handle_open_cmd_version))
        .await;

    // these are restricted (privileged)
    bot.register_privmsg_priv(&config.cmd_dumpacl, into_msg_handler(handle_priv_cmd_dump_acl))
        .await;
    bot.register_privmsg_priv(&config.cmd_join, into_msg_handler(handle_priv_cmd_join))
        .await;
    bot.register_privmsg_priv(&config.cmd_nick, into_msg_handler(handle_priv_cmd_nick))
        .await;
    bot.register_privmsg_priv(&config.cmd_reload, into_msg_handler(handle_priv_cmd_reload))
        .await;
    bot.register_privmsg_priv(&config.cmd_say, into_msg_handler(handle_priv_cmd_say))
        .await;

    Ok(())
}

// Process channel join messages here and return true only if something was reacted upon
async fn handle_join(bot: Arc<IrcBot>, cmd: Command) -> anyhow::Result<bool> {
    // We get called for all commands, this filter out only JOIN, otherwise bail out
    let channel = match cmd {
        Command::JOIN(ch, _, _) => ch,
        _ => return Ok(false),
    };

    let (nick, userhost, my_nick, acl_resp) = {
        let state = bot.state.read().await;
        let userhost = state.msg_userhost.clone();
        let config = bot.config.read().await;
        let acl_resp = config
            .auto_o_acl_rt
            .as_ref()
            .ok_or_else(|| anyhow!("no auto_o_acl_rt"))?
            .re_match(&userhost);
        (state.msg_nick.clone(), userhost, state.my_nick.clone(), acl_resp)
    };

    info!("JOIN <{nick}> {userhost} {channel}",);
    if bot.same_nick(&nick, &my_nick).await {
        // Ignore self join :p
        return Ok(false);
    }

    if let Some((i, s)) = acl_resp {
        info!("JOIN auto-op: ACL match {userhost} at index {i}: {s}",);
        bot.new_op(IrcOp::ModeOper(channel, nick)).await?;
        return Ok(true);
    }

    // we did nothing
    Ok(false)
}

async fn handle_open_cmd_invite(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    let (nick, userhost, channel) = {
        let (state, config) = (bot.state.read().await, bot.config.read().await);
        (
            state.msg_nick.clone(),
            state.msg_userhost.clone(),
            config.channel.clone(),
        )
    };

    let acl_resp_u = bot
        .config
        .read()
        .await
        .invite_bl_userhost_rt
        .as_ref()
        .ok_or_else(|| anyhow!("no invite_bl_userhost_rt"))?
        .re_match(&userhost);
    if let Some((i, s)) = acl_resp_u {
        info!("ACL match userhost \"{userhost}\" at index {i}: {s}");
        info!("Userhost {userhost} is blacklisted. No invite today.");
        return Ok(true);
    }

    let acl_resp_n = bot
        .config
        .read()
        .await
        .invite_bl_nick_rt
        .as_ref()
        .ok_or_else(|| anyhow!("no invite_bl_nick_rt"))?
        .re_match(&nick);
    if let Some((i, s)) = acl_resp_n {
        info!("ACL match nick \"{nick}\" at index {i}: {s}");
        info!("Nick {nick} is blacklisted. No invite today.");
        return Ok(true);
    }

    info!("Inviting {nick} to {channel}");
    bot.clone().new_op(IrcOp::Invite(nick, channel)).await
}

async fn handle_open_cmd_mode_o(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    let (nick, userhost, channel) = {
        let (state, config) = (bot.state.read().await, bot.config.read().await);
        (
            state.msg_nick.clone(),
            state.msg_userhost.clone(),
            config.channel.clone(),
        )
    };

    let now1 = Utc::now();
    let acl_resp = bot
        .config
        .read()
        .await
        .mode_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow!("no mode_o_acl_rt"))?
        .re_match(&userhost);
    debug!(
        "ACL check took {} µs.",
        Utc::now().signed_duration_since(now1).num_microseconds().unwrap_or(-1)
    );

    match acl_resp {
        Some((i, s)) => {
            info!("ACL match {userhost} at index {i}: {s}");
            bot.new_op(IrcOp::ModeOper(channel, nick)).await
        }
        None => {
            info!("ACL check failed for {userhost}. Fallback +v.");
            bot.new_op(IrcOp::ModeVoice(channel, nick)).await
        }
    }
}

async fn handle_open_cmd_mode_v(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    let nick = bot.state.read().await.msg_nick.clone();
    let channel = bot.config.read().await.channel.clone();
    bot.new_op(IrcOp::ModeVoice(channel, nick)).await
}

async fn handle_open_cmd_version(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    let nick = bot.state.read().await.msg_nick.clone();
    bot.new_msg(&nick, concat!(env!("CARGO_PKG_NAME"), " v", env!("CARGO_PKG_VERSION")))
        .await
}

async fn handle_priv_cmd_dump_acl(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    info!("Dumping ACLs");
    let nick = bot.state.read().await.msg_nick.clone();
    let staged_msgs = {
        let config = bot.config.read().await;
        let mode_o_acl = config
            .mode_o_acl_rt
            .as_ref()
            .ok_or_else(|| anyhow!("no mode_o_acl_rt"))?
            .patterns()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let auto_o_acl = config
            .auto_o_acl_rt
            .as_ref()
            .ok_or_else(|| anyhow!("no auto_o_acl_rt"))?
            .patterns()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let mut staged_msgs = Vec::with_capacity(mode_o_acl.len() + auto_o_acl.len() + 4);
        staged_msgs.push("My +o ACL:".to_string());
        staged_msgs.extend(mode_o_acl);
        staged_msgs.push("<EOF>".to_string());
        staged_msgs.push("My auto +o ACL:".to_string());
        staged_msgs.extend(auto_o_acl);
        staged_msgs.push("<EOF>".to_string());
        staged_msgs
    };

    for staged_msg in staged_msgs {
        bot.clone().new_msg(&nick, &staged_msg).await?;
    }
    Ok(true)
}

async fn handle_priv_cmd_join(bot: Arc<IrcBot>, _: String, _: String, new_chan: String) -> anyhow::Result<bool> {
    info!("Trying to join channel {new_chan}");
    bot.new_op(IrcOp::Join(new_chan)).await
}

async fn handle_priv_cmd_nick(bot: Arc<IrcBot>, _: String, _: String, new_nick: String) -> anyhow::Result<bool> {
    info!("Trying to change nick to {new_nick}");
    bot.new_op(IrcOp::Nick(new_nick)).await
}

async fn handle_priv_cmd_reload(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    // *** Try reloading all runtime configs ***
    error!("*** RELOADING CONFIG ***");
    let nick = bot.state.read().await.msg_nick.clone();
    bot.new_op(IrcOp::Reload(nick)).await
}

async fn handle_priv_cmd_say(bot: Arc<IrcBot>, _: String, _: String, say: String) -> anyhow::Result<bool> {
    if say.starts_with('#')
        // channel was specified
        && let Some((channel, msg)) = say.split_once(' ')
    {
        bot.new_msg(channel, msg).await?;
        return Ok(true);
    }

    // use the configured (default) channel name
    let cfg_channel = bot.config.read().await.channel.clone();
    bot.new_msg(&cfg_channel, &say).await
}

// EOF
