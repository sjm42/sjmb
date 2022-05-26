// sjmb.rs

use futures::prelude::*;
use irc::client::prelude::*;
use log::*;
use structopt::StructOpt;

use sjmb::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut opts = OptsCommon::from_args();
    opts.finish()?;
    start_pgm(&opts, "sjmb");
    info!("Starting up");
    let cfg = ConfigCommon::new(&opts)?;
    debug!("Config:\n{:#?}", &cfg);

    let mut irc = Client::new(opts.irc_config).await?;
    // trace!("My IRC client is:\n{irc:#?}");
    irc.identify()?;

    let mut stream = irc.stream()?;
    while let Some(message) = stream.next().await.transpose()? {
        let mynick = irc.current_nickname();
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

        match message.command {
            Command::PRIVMSG(channel, text) => {
                if channel == mynick {
                    // This is a private msg
                    info!(
                        "*Privmsg from {} ({}@{}): {}",
                        &msg_nick, &msg_user, &msg_host, &text
                    );

                    if text == cfg.i_password {
                        info!("Inviting {} to {}", &msg_nick, &cfg.channel);
                        if let Err(e) = irc.send_invite(&msg_nick, &cfg.channel) {
                            error!("{e}");
                            continue;
                        }
                        irc.send_privmsg(&msg_nick, format!("You may join {} now.", &cfg.channel))
                            .ok();
                    } else if text == cfg.v_password {
                        info!("Giving voice on {} to {}", &cfg.channel, &msg_nick);
                        if let Err(e) = irc.send_mode(
                            &cfg.channel,
                            &[Mode::Plus(ChannelMode::Voice, Some(msg_nick.clone()))],
                        ) {
                            error!("{e}");
                            continue;
                        }
                        irc.send_privmsg(&msg_nick, "You got +v now.").ok();
                    } else if text == cfg.o_password {
                        info!("Giving ops on {} to {}", &cfg.channel, &msg_nick);
                        if let Err(e) = irc.send_mode(
                            &cfg.channel,
                            &[Mode::Plus(ChannelMode::Oper, Some(msg_nick.clone()))],
                        ) {
                            error!("{e}");
                            continue;
                        }
                        irc.send_privmsg(&msg_nick, "You got +o now.").ok();
                    } else if text.starts_with("say ") && msg_nick == cfg.owner {
                        let say = &text[4..];
                        info!("{} <{}> {}", &cfg.channel, mynick, say);
                        irc.send_privmsg(&cfg.channel, say).ok();
                    }
                } else {
                    // This is a channel msg
                    info!("{} <{}> {}", &channel, &msg_nick, &text);
                    if text.contains(mynick) {
                        let say = "beep boop wat?";
                        info!("{} <{}> {}", &channel, mynick, say);
                        irc.send_privmsg(&channel, say).ok();
                    }
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
// EOF
