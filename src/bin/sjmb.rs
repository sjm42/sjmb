// irssi-urlharvest.rs

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

    let mut o_nick = "NONE".to_string();
    let mut o_user = "NONE".to_string();
    let mut o_host = "NONE".to_string();
    let mut stream = irc.stream()?;
    while let Some(message) = stream.next().await.transpose()? {
        let mynick = irc.current_nickname();
        trace!("Got msg: {message:?}");
        if let Some(Prefix::Nickname(nick, user, host)) = message.prefix {
            o_nick = nick;
            o_user = user;
            o_host = host;
        } else {
            o_nick = "NONE".into();
            o_user = "NONE".into();
            o_host = "NONE".into();
        }

        match message.command {
            Command::PRIVMSG(channel, text) => {
                if channel == mynick {
                    // This is a private msg
                    info!(
                        "*Privmsg from {} ({}@{}): {}",
                        &o_nick, &o_user, &o_host, &text
                    );

                    if text == cfg.v_password {
                        info!("Giving voice on {} to {}", &cfg.channel, &o_nick);
                        if let Err(e) = irc.send_mode(
                            &cfg.channel,
                            &[Mode::Plus(ChannelMode::Voice, Some(o_nick.clone()))],
                        ) {
                            error!("{e}");
                        }
                        let _ = irc.send_privmsg(&o_nick, "You got +v now.");
                    } else if text == cfg.o_password {
                        info!("Giving ops on {} to {}", &cfg.channel, &o_nick);
                        if let Err(e) = irc.send_mode(
                            &cfg.channel,
                            &[Mode::Plus(ChannelMode::Oper, Some(o_nick.clone()))],
                        ) {
                            error!("{e}");
                        }
                        let _ = irc.send_privmsg(&o_nick, "You got +o now.");
                    } else if o_nick == cfg.owner && text.starts_with("say ") {
                        let say = &text[4..];
                        info!("{} <{}> {}", &cfg.channel, mynick, say);
                        let _ = irc.send_privmsg(&cfg.channel, say);
                    }
                } else {
                    // This is a channel msg
                    info!("{} <{}> {}", &channel, &o_nick, &text);
                    if text.contains(mynick) {
                        let say = "beep boop wat?";
                        info!("{} <{}> {}", &channel, mynick, say);
                        let _ = irc.send_privmsg(&channel, say);
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
