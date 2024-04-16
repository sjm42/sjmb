// bin/sjmb.rs

use chrono::*;
use clap::Parser;
use irc::client::prelude::*;
use tokio::time::{Duration, sleep};

use sjmb::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut opts = OptsCommon::parse();
    opts.finalize()?;
    opts.start_pgm(env!("CARGO_BIN_NAME"));

    let mut first_time = true;
    loop {
        if first_time {
            first_time = false;
        } else {
            error!("Sleeping 10s...");
            sleep(Duration::from_secs(10)).await;
            error!("Retrying start");
        }

        let mut ircbot = IrcBot::new(&opts).await?;
        bot_cmd_setup(&mut ircbot);

        if let Err(e) = ircbot.run().await {
            error!("{e}");
        }
        drop(ircbot);
    }
}

fn bot_cmd_setup(bot: &mut IrcBot) {
    bot.clear_handlers();

    // Register JOIN callback
    bot.register_irc_cmd(handle_join);

    // ### Register commands

    // these can be used by anyone
    bot.register_privmsg_open(bot.bot_cfg.cmd_invite.to_string(), handle_pcmd_invite);
    bot.register_privmsg_open(bot.bot_cfg.cmd_mode_o.to_string(), handle_pcmd_mode_o);
    bot.register_privmsg_open(bot.bot_cfg.cmd_mode_v.to_string(), handle_pcmd_mode_v);

    // these are restricted
    bot.register_privmsg_priv(bot.bot_cfg.cmd_dumpacl.to_string(), handle_pcmd_dumpacl);
    bot.register_privmsg_priv(bot.bot_cfg.cmd_join.to_string(), handle_pcmd_join);
    bot.register_privmsg_priv(bot.bot_cfg.cmd_nick.to_string(), handle_pcmd_nick);
    bot.register_privmsg_priv(bot.bot_cfg.cmd_reload.to_string(), handle_pcmd_reload);
    bot.register_privmsg_priv(bot.bot_cfg.cmd_say.to_string(), handle_pcmd_say);
}

// Process channel join messages here and return true only if something was reacted upon
fn handle_join(bot: &IrcBot, cmd: &irc::proto::Command) -> anyhow::Result<bool> {
    // We get called for all commands, this filter out only JOIN, otherwise bail out
    let channel = match cmd {
        Command::JOIN(ch, _, _) => ch,
        _ => return Ok(false),
    };

    let nick = bot.msg_nick();
    let userhost = bot.msg_userhost();

    info!("JOIN <{nick}> {userhost} {channel}",);
    if nick == bot.mynick() {
        // Ignore self join :p
        return Ok(false);
    }

    let now1 = Utc::now();
    let acl_resp = bot
        .bot_cfg
        .auto_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no auto_o_acl_rt"))?
        .re_match(userhost);
    debug!(
        "Auto-op acl check took {} µs.",
        Utc::now()
            .signed_duration_since(now1)
            .num_microseconds()
            .unwrap_or(-1)
    );

    if let Some((i, s)) = acl_resp {
        info!("JOIN auto-op: ACL match {userhost} at index {i}: {s}",);
        bot.new_op(IrcOp::ModeOper(channel.into(), nick.into()))?;
        return Ok(true);
    }

    // we did nothing
    Ok(false)
}

fn handle_pcmd_reload(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    // *** Try reloading all runtime configs ***
    error!("*** RELOADING CONFIG ***");
    let nick = bot.msg_nick().to_string();
    match bot.reload() {
        Ok(ret) => {
            // reinitialize command handlers
            bot_cmd_setup(bot);
            bot.new_msg(nick, "*** Reload successful.")?;
            Ok(ret)
        }
        Err(e) => {
            bot.new_msg(nick, e.to_string())?;
            Err(e)
        }
    }
}

fn handle_pcmd_dumpacl(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    info!("Dumping ACLs");
    let nick = bot.msg_nick();

    bot.new_msg(nick, "My +o ACL:")?;
    for s in &bot
        .bot_cfg
        .mode_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no mode_o_acl_rt"))?
        .acl_str
    {
        bot.new_msg(nick, s)?;
    }
    bot.new_msg(nick, "<EOF>")?;

    bot.new_msg(nick, "My auto +o ACL:")?;
    for s in &bot
        .bot_cfg
        .auto_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no auto_o_acl_rt"))?
        .acl_str
    {
        bot.new_msg(nick, s)?;
    }
    bot.new_msg(nick, "<EOF>")?;

    Ok(true)
}

fn handle_pcmd_say(bot: &mut IrcBot, _: &str, _: &str, say: &str) -> anyhow::Result<bool> {
    if say.starts_with('#') {
        // channel was specified
        if let Some((channel, msg)) = say.split_once(' ') {
            bot.new_msg(channel, msg)?;
            return Ok(true);
        }
    }
    let cfg_channel = &bot.bot_cfg.channel;
    bot.new_msg(cfg_channel, say)?;
    Ok(true)
}

fn handle_pcmd_nick(bot: &mut IrcBot, _: &str, _: &str, newnick: &str) -> anyhow::Result<bool> {
    info!("Trying to change nick to {newnick}");
    bot.new_op(IrcOp::Nick(newnick.into()))?;
    Ok(true)
}

fn handle_pcmd_join(bot: &mut IrcBot, _: &str, _: &str, newchan: &str) -> anyhow::Result<bool> {
    info!("Trying to join channel {newchan}");
    bot.new_op(IrcOp::Join(newchan.into()))?;
    Ok(true)
}

// These commands are unholy because the config is massaged inside general bot config

fn handle_pcmd_invite(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    let nick = bot.msg_nick();
    let channel = bot.bot_cfg.channel.to_string();
    info!("Inviting {nick} to {channel}");
    bot.new_op(IrcOp::Invite(nick.into(), channel))?;
    Ok(true)
}

fn handle_pcmd_mode_o(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    let nick = bot.msg_nick();
    let userhost = bot.msg_userhost();
    let channel = &bot.bot_cfg.channel;

    let now1 = Utc::now();
    let acl_resp = bot
        .bot_cfg
        .mode_o_acl_rt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no mode_o_acl_rt"))?
        .re_match(userhost);
    debug!(
        "ACL check took {} µs.",
        Utc::now()
            .signed_duration_since(now1)
            .num_microseconds()
            .unwrap_or(-1)
    );

    match acl_resp {
        Some((i, s)) => {
            info!("ACL match {userhost} at index {i}: {s}");
            bot.new_op(IrcOp::ModeOper(channel.into(), nick.into()))
        }
        None => {
            info!("ACL check failed for {userhost}. Fallback +v.");
            bot.new_op(IrcOp::ModeVoice(channel.into(), nick.into()))
        }
    }
}

fn handle_pcmd_mode_v(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    let nick = bot.msg_nick();
    let channel = &bot.bot_cfg.channel;
    bot.new_op(IrcOp::ModeVoice(channel.into(), nick.into()))
}

// EOF
