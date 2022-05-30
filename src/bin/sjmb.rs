// sjmb.rs

use futures::prelude::*;
use irc::client::prelude::*;
use log::*;
use std::fmt::Display;
use structopt::StructOpt;

use sjmb::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut opts = OptsCommon::from_args();
    opts.finish()?;
    start_pgm(&opts, "sjmb");
    info!("Starting up");

    let mut bot_cfg = BotRuntimeConfig::new(&opts)?;

    let mut irc = Client::new(&opts.irc_config).await?;
    // trace!("My IRC client is:\n{irc:#?}");
    irc.identify()?;

    let mut stream = irc.stream()?;
    while let Some(message) = stream.next().await.transpose()? {
        let cfg = &bot_cfg.common;
        let mynick = irc.current_nickname();
        let cfg_channel = &cfg.channel;
        trace!("Got msg: {message:?}");
        let msg_nick;
        let msg_user;
        let msg_host;
        if let Some(Prefix::Nickname(nick, user, host)) = message.prefix {
            msg_nick = nick;
            msg_user = user;
            msg_host = host;
        } else {
            msg_nick = "NONE".into();
            msg_user = "NONE".into();
            msg_host = "NONE".into();
        }
        let userhost = format!("{msg_user}@{msg_host}");

        match message.command {
            Command::PRIVMSG(channel, text) => {
                if channel == mynick {
                    // This is a private msg
                    info!(
                        "*** Privmsg from {} ({}@{}): {}",
                        &msg_nick, &msg_user, &msg_host, &text
                    );

                    if msg_nick == cfg.owner {
                        // Owner commands

                        if text == "reload" {
                            // *** Try reloading all runtime configs ***
                            error!("*** RELOADING CONFIG ***");
                            let new_cfg = match BotRuntimeConfig::new(&opts) {
                                Ok(c) => c,
                                Err(e) => {
                                    error!("Could not parse runtime config:\n{e}");
                                    error!("*** Reload failed.");
                                    continue;
                                }
                            };
                            info!("*** Reload successful.");
                            bot_cfg = new_cfg;
                        } else if text == "acl" {
                            info!("Dumping ACL");
                            irc.send_privmsg(&msg_nick, "My +o ACL:").ok();
                            for s in &bot_cfg.o_acl.acl {
                                irc.send_privmsg(&msg_nick, s).ok();
                            }
                            irc.send_privmsg(&msg_nick, "<EOF>").ok();
                        } else if let Some(say) = text.strip_prefix("say ") {
                            info!("{cfg_channel} <{mynick}> {say}");
                            irc.send_privmsg(cfg_channel, say).ok();
                        }
                    } else {
                        // Public commands

                        if text == cfg.cmd_invite {
                            info!("Inviting {msg_nick} to {cfg_channel}");
                            if let Err(e) = irc.send_invite(&msg_nick, cfg_channel) {
                                error!("{e}");
                                continue;
                            }
                            // irc.send_privmsg(&msg_nick, format!("You may join {cfg_channel} now.")).ok();
                        } else if text == cfg.cmd_mode_v {
                            mode_v(&irc, cfg_channel, &msg_nick);
                        } else if text == cfg.cmd_mode_o {
                            match bot_cfg.acl_match(&userhost) {
                                Some((i, s)) => {
                                    info!("ACL match {userhost} at line {}: {}", i + 1, &s);
                                    info!("Giving ops on {cfg_channel} to {msg_nick}");
                                    if let Err(e) = irc.send_mode(
                                        cfg_channel,
                                        &[Mode::Plus(ChannelMode::Oper, Some(msg_nick.clone()))],
                                    ) {
                                        error!("{e}");
                                        continue;
                                    }
                                }
                                None => {
                                    info!("ACL check failed for {userhost}. Fallback +v on {cfg_channel} to {msg_nick}");
                                    mode_v(&irc, cfg_channel, &msg_nick);
                                }
                            }

                            // irc.send_privmsg(&msg_nick, "You got +o now.").ok();
                        }
                    }
                } else {
                    // This is a channel msg
                    debug!("{channel} <{msg_nick}> {text}");
                    /*
                    if text.contains(mynick) {
                        let say = "Hmm?";
                        info!("{channel} <{mynick}> {say}");
                        irc.send_privmsg(&channel, say).ok();
                    }
                    */
                }
            }
            Command::Response(resp, v) => {
                debug!("Got response type {resp:?} contents: {v:?}");
            }
            cmd => {
                debug!("Unhandled command: {cmd:?}")
            }
        }
    }
    Ok(())
}

pub fn mode_v<S>(irc: &Client, channel: S, nick: S)
where
    S: AsRef<str> + Display,
{
    info!("Giving voice on {channel} to {nick}");
    if let Err(e) = irc.send_mode(
        channel,
        &[Mode::Plus(ChannelMode::Voice, Some(nick.to_string()))],
    ) {
        error!("{e}");
        // return;
    }
    // irc.send_privmsg(nick.as_ref(), "You got +v now.").ok();
}
// EOF
