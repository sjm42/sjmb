// bin/sjmb.rs

use clap::Parser;

use sjmb::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut opts = OptsCommon::parse();
    opts.finalize()?;
    opts.start_pgm(env!("CARGO_BIN_NAME"));

    loop {
        let (bot, irc_stream) = IrcBot::new(&opts).await?;
        let ircbot = Arc::new(bot);
        bot_cmd_setup(ircbot.clone()).await?;

        if let Err(e) = ircbot.run(irc_stream).await {
            error!("{e}");
        }

        error!("Sleeping 10s...");
        sleep(Duration::from_secs(10)).await;
        error!("Retrying start");
    }
}

async fn bot_cmd_setup(bot: Arc<IrcBot>) -> anyhow::Result<()> {
    bot.clear_handlers().await;

    // Register JOIN callback
    bot.register_irc_cmd(into_cmd_handler(handle_join)).await;

    // ### Register commands
    let config = bot.config.read().await;

    // these can be used by anyone (open)
    bot.register_privmsg_open(&config.cmd_invite, into_msg_handler(handle_open_cmd_invite))
        .await;
    bot.register_privmsg_open(&config.cmd_mode_o, into_msg_handler(handle_open_cmd_mode_o))
        .await;
    bot.register_privmsg_open(&config.cmd_mode_v, into_msg_handler(handle_open_cmd_mode_v))
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
            .ok_or_else(|| anyhow::anyhow!("no auto_o_acl_rt"))?
            .re_match(&userhost);
        (state.msg_nick.clone(), userhost, state.my_nick.clone(), acl_resp)
    };

    info!("JOIN <{nick}> {userhost} {channel}",);
    if nick == my_nick {
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
        .ok_or_else(|| anyhow::anyhow!("no invite_bl_userhost_rt"))?
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
        .ok_or_else(|| anyhow::anyhow!("no invite_bl_nick_rt"))?
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
        .ok_or_else(|| anyhow::anyhow!("no mode_o_acl_rt"))?
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

async fn handle_priv_cmd_dump_acl(bot: Arc<IrcBot>, _: String, _: String, _: String) -> anyhow::Result<bool> {
    info!("Dumping ACLs");
    let nick = bot.state.read().await.msg_nick.clone();

    bot.clone().new_msg(&nick, "My +o ACL:").await?;
    for s in &bot
        .config
        .read()
        .await
        .mode_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no mode_o_acl_rt"))?
        .acl_str
    {
        bot.clone().new_msg(&nick, s).await?;
    }
    bot.clone().new_msg(&nick, "<EOF>").await?;

    bot.clone().new_msg(&nick, "My auto +o ACL:").await?;
    for s in &bot
        .config
        .read()
        .await
        .auto_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no auto_o_acl_rt"))?
        .acl_str
    {
        bot.clone().new_msg(&nick, s).await?;
    }
    bot.new_msg(&nick, "<EOF>").await
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
    match bot.clone().reload().await {
        Ok(ret) => {
            bot.new_msg(&nick, "*** Reload successful.").await?;
            Ok(ret)
        }
        Err(e) => {
            bot.new_msg(&nick, &format!("*** Reload failed: {}", e)).await?;
            Err(e)
        }
    }
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
