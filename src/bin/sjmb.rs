// sjmb.rs

use chrono::*;
use irc::client::prelude::*;
use log::*;
use sjmb::*;
use std::{thread, time};
use structopt::StructOpt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut opts = OptsCommon::from_args();
    opts.finish()?;
    opts.start_pgm("sjmb");
    info!("Starting up");

    never_gonna_give_you_up(opts).await;
    Ok(())
}

async fn never_gonna_give_you_up(opts: OptsCommon) -> ! {
    let mut first_time = true;
    loop {
        if first_time {
            first_time = false;
        } else {
            error!("Sleeping 10s...");
            thread::sleep(time::Duration::from_secs(10));
            error!("Retrying start");
        }

        let mut ircbot = IrcBot::new(&opts).await.unwrap();
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

    // Register commands
    bot.register_privmsg_priv("reload", handle_pcmd_reload);
    bot.register_privmsg_priv("dumpacl", handle_pcmd_dumpacl);
    bot.register_privmsg_priv("say", handle_pcmd_say);
    bot.register_privmsg_priv("nick", handle_pcmd_nick);
    bot.register_privmsg_priv("join", handle_pcmd_join);

    // These commands are unholy because the config is massaged inside general bot config.
    // Anyway, the public commands are configurable.
    bot.register_privmsg_open(bot.bot_cfg.cmd_invite.to_string(), handle_pcmd_invite);
    bot.register_privmsg_open(bot.bot_cfg.cmd_mode_o.to_string(), handle_pcmd_mode_o);
    bot.register_privmsg_open(bot.bot_cfg.cmd_mode_v.to_string(), handle_pcmd_mode_v);
}

// Process channel join messages here and return true only if something was reacted upon
fn handle_join(bot: &IrcBot, cmd: &irc::proto::Command) -> anyhow::Result<bool> {
    let ch = match cmd {
        Command::JOIN(ch, _, _) => ch,
        _ => return Ok(false),
    };

    info!(
        "JOIN <{nick}> {userhost} {ch}",
        nick = bot.msg_nick(),
        userhost = bot.msg_userhost(),
    );
    if bot.msg_nick() == bot.mynick() {
        // Ignore self join :p
        return Ok(false);
    }

    let now1 = Utc::now();
    let acl_resp = bot
        .bot_cfg
        .auto_o_acl_rt
        .as_ref()
        .unwrap()
        .re_match(&bot.msg_userhost());
    debug!(
        "ACL check took {} µs.",
        Utc::now()
            .signed_duration_since(now1)
            .num_microseconds()
            .unwrap_or(0)
    );

    if let Some((i, s)) = acl_resp {
        info!(
            "JOIN auto-op: ACL match {userhost} at index {i}: {s}",
            userhost = bot.msg_userhost()
        );
        bot.mode_o(ch, &bot.msg_nick())?;
        return Ok(true);
    }

    // we did nothing
    Ok(false)
}

fn handle_pcmd_reload(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    // *** Try reloading all runtime configs ***
    error!("*** RELOADING CONFIG ***");
    let nick = bot.msg_nick();
    match bot.reload() {
        Ok(ret) => {
            // reinitialize command handlers
            bot_cmd_setup(bot);
            bot.send_msg(&nick, "*** Reload successful.")?;
            Ok(ret)
        }
        Err(e) => {
            bot.send_msg(&nick, e.to_string())?;
            Err(e)
        }
    }
}

fn handle_pcmd_dumpacl(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    info!("Dumping ACLs");
    let nick = bot.msg_nick();

    bot.send_msg(&nick, "My +o ACL:")?;
    for s in &bot.bot_cfg.mode_o_acl_rt.as_ref().unwrap().acl {
        bot.send_msg(&nick, s)?;
    }
    bot.send_msg(&nick, "<EOF>")?;

    bot.send_msg(&nick, "My auto +o ACL:")?;
    for s in &bot.bot_cfg.auto_o_acl_rt.as_ref().unwrap().acl {
        bot.send_msg(&nick, s)?;
    }
    bot.send_msg(&nick, "<EOF>")?;

    Ok(true)
}

fn handle_pcmd_say(bot: &mut IrcBot, _: &str, _: &str, say: &str) -> anyhow::Result<bool> {
    if say.starts_with('#') {
        // channel was specified
        if let Some((channel, msg)) = say.split_once(' ') {
            bot.send_msg(channel, msg)?;
            return Ok(true);
        }
    }
    let cfg_channel = &bot.bot_cfg.channel;
    bot.send_msg(cfg_channel, say)?;
    Ok(true)
}

fn handle_pcmd_nick(bot: &mut IrcBot, _: &str, _: &str, newnick: &str) -> anyhow::Result<bool> {
    info!("Trying to change nick to {newnick}");
    bot.irc.send(Command::NICK(newnick.into()))?;
    Ok(true)
}

fn handle_pcmd_join(bot: &mut IrcBot, _: &str, _: &str, newchan: &str) -> anyhow::Result<bool> {
    info!("Trying to join channel {newchan}");
    bot.irc.send(Command::JOIN(newchan.into(), None, None))?;
    Ok(true)
}

// These commands are unholy because the config is massaged inside general bot config

fn handle_pcmd_invite(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    let nick = bot.msg_nick();
    let channel = &bot.bot_cfg.channel;
    info!("Inviting {nick} to {channel}");
    match bot.irc.send_invite(nick, channel) {
        Err(e) => {
            error!("{e}");
            Err(e.into())
        }
        Ok(_) => Ok(true),
    }
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
        .unwrap()
        .re_match(&userhost);
    debug!(
        "ACL check took {} µs.",
        Utc::now()
            .signed_duration_since(now1)
            .num_microseconds()
            .unwrap_or(0)
    );

    match acl_resp {
        Some((i, s)) => {
            info!("ACL match {userhost} at index {i}: {s}");
            bot.mode_o(channel, nick)
        }
        None => {
            info!("ACL check failed for {userhost}. Fallback +v.");
            bot.mode_v(channel, nick)
        }
    }
}

fn handle_pcmd_mode_v(bot: &mut IrcBot, _: &str, _: &str, _: &str) -> anyhow::Result<bool> {
    let nick = bot.msg_nick();
    let channel = &bot.bot_cfg.channel;
    bot.mode_v(channel, nick)
}

// EOF
