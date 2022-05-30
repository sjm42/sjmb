// sjmb.rs

use chrono::*;
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
                            match BotRuntimeConfig::new(&opts) {
                                Ok(c) => {
                                    info!("*** Reload successful.");
                                    bot_cfg = c;
                                }
                                Err(e) => {
                                    error!("Could not parse runtime config:\n{e}");
                                    error!("*** Reload failed.");
                                }
                            };
                            continue;
                        } else if text == "mode_o_acl" {
                            info!("Dumping ACL");
                            irc.send_privmsg(&msg_nick, "My +o ACL:").ok();
                            for s in &bot_cfg.mode_o_acl.acl_str {
                                irc.send_privmsg(&msg_nick, s).ok();
                            }
                            irc.send_privmsg(&msg_nick, "<EOF>").ok();
                            continue;
                        } else if let Some(say) = text.strip_prefix("say ") {
                            info!("{cfg_channel} <{mynick}> {say}");
                            irc.send_privmsg(cfg_channel, say).ok();
                            continue;
                        }
                    }

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
                        let now1 = Utc::now();
                        let acl_resp = bot_cfg.mode_o_acl.re_match(&userhost);
                        info!(
                            "ACL check took {} µs.",
                            Utc::now()
                                .signed_duration_since(now1)
                                .num_microseconds()
                                .unwrap_or(0)
                        );

                        match acl_resp {
                            Some((i, s)) => {
                                info!("ACL match {userhost} at index {i}: {s}");
                                info!("Giving ops on {cfg_channel} to {msg_nick}");
                                mode_o(&irc, cfg_channel, &msg_nick);
                            }
                            None => {
                                info!("ACL check failed for {userhost}. Fallback +v on {cfg_channel} to {msg_nick}");
                                mode_v(&irc, cfg_channel, &msg_nick);
                            }
                        }

                        // irc.send_privmsg(&msg_nick, "You got +o now.").ok();
                    }

                    // All other private messages are ignored
                    continue;
                }

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
            Command::Response(resp, v) => {
                debug!("Got response type {resp:?} contents: {v:?}");
            }
            Command::JOIN(ch, _, _) => {
                info!("JOIN <{msg_nick}> {userhost} {ch}");
                let now1 = Utc::now();
                let acl_resp = bot_cfg.auto_o_acl.re_match(&userhost);
                info!(
                    "JOIN ACL check took {} µs.",
                    Utc::now()
                        .signed_duration_since(now1)
                        .num_microseconds()
                        .unwrap_or(0)
                );

                if let Some((i, s)) = acl_resp {
                    info!("JOIN ACL match {userhost} at index {i}: {s}");
                    info!("Giving ops on {cfg_channel} to {msg_nick}");
                    mode_o(&irc, cfg_channel, &msg_nick);
                }
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
    info!("Giving +v on {channel} to {nick}");
    if let Err(e) = irc.send_mode(
        channel,
        &[Mode::Plus(ChannelMode::Voice, Some(nick.to_string()))],
    ) {
        error!("{e}");
        // return;
    }
    // irc.send_privmsg(nick.as_ref(), "You got +v now.").ok();
}

pub fn mode_o<S>(irc: &Client, channel: S, nick: S)
where
    S: AsRef<str> + Display,
{
    info!("Giving +o on {channel} to {nick}");
    if let Err(e) = irc.send_mode(
        channel,
        &[Mode::Plus(ChannelMode::Oper, Some(nick.to_string()))],
    ) {
        error!("{e}");
        // return;
    }
    // irc.send_privmsg(nick.as_ref(), "You got +v now.").ok();
}

// EOF
