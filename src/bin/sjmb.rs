// sjmb.rs

use chrono::*;
use futures::prelude::*;
use irc::client::prelude::*;
use log::*;
use std::{fmt::Display, thread, time};
use structopt::StructOpt;
use url::Url;
use webpage::{Webpage, WebpageOptions}; // provides `try_next`

use sjmb::*;

pub struct IrcState {
    irc: Client,
    opts: OptsCommon,
    bot_cfg: BotConfig,
    mynick: String,
    msg_nick: String,
    msg_user: String,
    msg_host: String,
    userhost: String,
}

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

        let bot_cfg = match BotConfig::new(&opts) {
            Ok(b) => b,
            Err(e) => {
                error!("{e}");
                continue;
            }
        };

        let irc = match Client::new(&opts.irc_config).await {
            Ok(c) => c,
            Err(e) => {
                error!("{e}");
                continue;
            }
        };

        if let Err(e) = irc.identify() {
            error!("{e}");
            drop(irc);
            continue;
        }

        let mynick = irc.current_nickname().to_string();
        let istate = IrcState {
            irc,
            opts: opts.clone(),
            bot_cfg,
            mynick,
            msg_nick: "NONE".into(),
            msg_user: "NONE".into(),
            msg_host: "NONE".into(),
            userhost: "NONE@NONE".into(),
        };

        if let Err(e) = run_main_loop(istate).await {
            error!("{e}");
            continue;
        }
    }
}

async fn run_main_loop(mut istate: IrcState) -> anyhow::Result<()> {
    let mut stream = istate.irc.stream()?;
    while let Some(message) = stream.next().await.transpose()? {
        trace!("Got msg: {message:?}");
        let mynick = &istate.mynick;

        let msg_nick;
        let msg_user;
        let msg_host;

        if let Some(Prefix::Nickname(nick, user, host)) = message.prefix {
            (msg_nick, msg_user, msg_host) = (nick, user, host);
        } else {
            (msg_nick, msg_user, msg_host) = ("NONE".into(), "NONE".into(), "NONE".into());
        }
        istate.msg_nick = msg_nick.clone();
        istate.msg_user = msg_user.clone();
        istate.msg_host = msg_host.clone();

        let userhost = format!("{msg_user}@{msg_host}");
        istate.userhost = userhost.clone();

        match message.command {
            Command::Response(resp, v) => {
                debug!("Got response type {resp:?} contents: {v:?}");
            }

            Command::JOIN(ch, _, _) => {
                if let Err(e) = handle_join(&istate, &ch) {
                    error!("JOIN handling failed: {e}");
                }
            }

            Command::PRIVMSG(channel, msg) => {
                if channel == *mynick {
                    if let Err(e) = handle_private_msg(&mut istate, &msg) {
                        error!("PRIVMSG handling failed: {e}");
                    }
                } else if let Err(e) = handle_channel_msg(&istate, &channel, &msg).await {
                    error!("Channel msg handling failed: {e}");
                }
            }

            Command::NICK(newnick) => {
                debug!("NICK: {msg_nick} USER: {msg_user} HOST: {msg_host} NEW NICK: {newnick}");
                if msg_nick == *mynick {
                    info!("My NEW nick: {newnick}");
                    istate.mynick = newnick;
                }
            }

            cmd => {
                debug!("Unhandled command: {cmd:?}")
            }
        }
    }

    Ok(())
}

// Process channel join messages here and return true only if something was reacted upon
fn handle_join(st: &IrcState, ch: &str) -> anyhow::Result<bool> {
    info!(
        "JOIN <{nick}> {userhost} {ch}",
        nick = &st.msg_nick,
        userhost = &st.userhost,
    );
    if st.msg_nick == st.mynick {
        // Ignore self join :p
        return Ok(false);
    }

    let now1 = Utc::now();
    let acl_resp = st
        .bot_cfg
        .auto_o_acl_rt
        .as_ref()
        .unwrap()
        .re_match(&st.userhost);
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
            userhost = &st.userhost
        );
        mode_o(&st.irc, ch, &st.msg_nick)?;
        return Ok(true);
    }

    // we did nothing
    Ok(false)
}

// Process private messages here and return true only if something was reacted upon
fn handle_private_msg(st: &mut IrcState, msg: &str) -> anyhow::Result<bool> {
    let cfg = &st.bot_cfg;

    info!(
        "*** Privmsg from {} ({}@{}): {}",
        &st.msg_nick, &st.msg_user, &st.msg_host, msg
    );

    if let Some(true) = cfg.privileged_nicks.get(&st.msg_nick) {
        // Handle privileged commands
        if handle_cmd_privileged(st, msg)? {
            // a command was found and executed if true was returned
            return Ok(true);
        }
    }

    // Handle public commands
    if handle_cmd_public(st, msg)? {
        // a command was found and executed if true was returned
        return Ok(true);
    }

    // All other private messages are ignored
    Ok(false)
}

// Process privileged commands here and return true only if something was reacted upon
fn handle_cmd_privileged(st: &mut IrcState, msg: &str) -> anyhow::Result<bool> {
    let cfg = &st.bot_cfg;

    if let Some(say) = msg.strip_prefix("say ") {
        if say.starts_with('#') {
            // channel was specified
            if let Some((channel, msg)) = say.split_once(' ') {
                info!("{channel} <{mynick}> {msg}", mynick = st.mynick);
                st.irc.send_privmsg(channel, msg)?;
                return Ok(true);
            }
        }
        let cfg_channel = &cfg.channel;
        info!("{cfg_channel} <{mynick}> {say}", mynick = st.mynick);
        st.irc.send_privmsg(cfg_channel, say)?;
        return Ok(true);
    }

    if let Some(newnick) = msg.strip_prefix("nick ") {
        info!("Trying to change nick to {newnick}");
        st.irc.send(Command::NICK(newnick.into()))?;
    }

    if msg == "reload" {
        // *** Try reloading all runtime configs ***
        error!("*** RELOADING CONFIG ***");
        match BotConfig::new(&st.opts) {
            Ok(c) => {
                st.bot_cfg = c;
                let msg = "*** Reload successful.";
                info!("{msg}");
                st.irc.send_privmsg(&st.msg_nick, msg)?;
            }
            Err(e) => {
                let msg = format!(
                    "Could not parse runtime config {c}: {e}",
                    c = &st.opts.bot_config
                );
                error!("{msg}");
                st.irc.send_privmsg(&st.msg_nick, &msg)?;
                let msg = "*** Reload failed.";
                error!("{msg}");
                st.irc.send_privmsg(&st.msg_nick, msg)?;
            }
        };
        return Ok(true);
    }

    if msg == "mode_o_acl" {
        info!("Dumping ACL");
        st.irc.send_privmsg(&st.msg_nick, "My +o ACL:")?;
        for s in &st.bot_cfg.mode_o_acl_rt.as_ref().unwrap().acl {
            st.irc.send_privmsg(&st.msg_nick, s)?;
        }
        st.irc.send_privmsg(&st.msg_nick, "<EOF>")?;
        return Ok(true);
    }

    // did not recognize any command
    Ok(false)
}

// Process "public" commands here and return true only if something was reacted upon
fn handle_cmd_public(st: &mut IrcState, msg: &str) -> anyhow::Result<bool> {
    let cfg = &st.bot_cfg;

    if msg == cfg.cmd_invite {
        info!(
            "Inviting {nick} to {channel}",
            nick = &st.msg_nick,
            channel = &cfg.channel
        );
        if let Err(e) = st.irc.send_invite(&st.msg_nick, &cfg.channel) {
            error!("{e}");
            return Err(e.into());
        }
        // irc.send_privmsg(&msg_nick, format!("You may join {cfg_channel} now.")).ok();
        return Ok(true);
    }

    if msg == cfg.cmd_mode_v {
        mode_v(&st.irc, &cfg.channel, &st.msg_nick)?;
        return Ok(true);
    }

    if msg == cfg.cmd_mode_o {
        let now1 = Utc::now();
        let acl_resp = st
            .bot_cfg
            .mode_o_acl_rt
            .as_ref()
            .unwrap()
            .re_match(&st.userhost);
        debug!(
            "ACL check took {} µs.",
            Utc::now()
                .signed_duration_since(now1)
                .num_microseconds()
                .unwrap_or(0)
        );

        match acl_resp {
            Some((i, s)) => {
                info!(
                    "ACL match {userhost} at index {i}: {s}",
                    userhost = st.userhost
                );
                mode_o(&st.irc, &cfg.channel, &st.msg_nick)?;
            }
            None => {
                info!(
                    "ACL check failed for {userhost}. Fallback +v.",
                    userhost = &st.userhost
                );
                mode_v(&st.irc, &cfg.channel, &st.msg_nick)?;
            }
        }

        // irc.send_privmsg(&msg_nick, "You got +o now.").ok();
        return Ok(true);
    }

    // did not recognize any command
    Ok(false)
}

// Process channel messages here and return true only if something was reacted upon
async fn handle_channel_msg(st: &IrcState, channel: &str, msg: &str) -> anyhow::Result<bool> {
    let cfg = &st.bot_cfg;

    debug!("{channel} <{nick}> {msg}", nick = st.msg_nick);

    if let Some(url_cmd) = msg.strip_prefix('!') {
        if let Some((cmd, arg)) = url_cmd.split_once(' ') {
            if let Some(c) = cfg.url_cmd_list.get(cmd) {
                // phew we found a command to execute!

                // render URL to retrieve
                let mut ctx = tera::Context::new();
                ctx.insert("arg", arg);
                let url = cfg.url_cmd_tera.as_ref().unwrap().render(cmd, &ctx)?;
                info!("URL cmd: !{url_cmd} --> {url}");

                let client = reqwest::Client::builder()
                    .connect_timeout(time::Duration::new(5, 0))
                    .timeout(time::Duration::new(10, 0))
                    .build()?;

                let body = client.get(&url).send().await?.text().await?;
                debug!("Got body:\n{body}");

                for res_cap in c.output_filter_re.as_ref().unwrap().captures_iter(&body) {
                    let res_str = &res_cap[1];
                    let say = format!("{url_cmd} --> {res_str}");
                    info!("{channel} <{mynick}> {say}", mynick = st.mynick);
                    st.irc.send_privmsg(&channel, say)?;
                }

                return Ok(true);
            }
        }
    }

    // Are we supposed to detect urls and show titles on this channel?
    if let Some(true) = cfg.url_fetch_channels.get(channel) {
        let mut found_url = false;
        for url_cap in cfg
            .url_regex_re
            .as_ref()
            .unwrap()
            .captures_iter(msg.as_ref())
        {
            found_url = true;
            let url_s = &url_cap[1];
            if let Ok(url) = Url::parse(url_s) {
                // Now we should have a canonical url, IDN handled etc.
                let url_c = String::from(url);
                info!("*** detected url: {url_c}");
                info!("Fetching URL {url_c}");

                let webpage_opts = WebpageOptions {
                    allow_insecure: true,
                    timeout: time::Duration::new(5, 0),
                    ..Default::default()
                };

                if let Ok(pageinfo) = Webpage::from_url(&url_c, webpage_opts) {
                    if let Some(title) = pageinfo.html.title {
                        // ignore titles that are just the url repeated
                        if title != url_s {
                            let say = format!("\"{title}\"");
                            info!("{channel} <{mynick}> {say}", mynick = st.mynick);
                            st.irc.send_privmsg(&channel, say)?;
                        }
                    }
                }
            }
        }
        return Ok(found_url);
    }

    Ok(false)
}

pub fn mode_v<S1, S2>(irc: &Client, channel: S1, nick: S2) -> anyhow::Result<()>
where
    S1: AsRef<str> + Display,
    S2: AsRef<str> + Display,
{
    info!("Giving +v on {channel} to {nick}");
    if let Err(e) = irc.send_mode(
        channel,
        &[Mode::Plus(ChannelMode::Voice, Some(nick.to_string()))],
    ) {
        error!("{e}");
        return Err(e.into());
    }
    // irc.send_privmsg(nick.as_ref(), "You got +v now.").ok();
    Ok(())
}

pub fn mode_o<S1, S2>(irc: &Client, channel: S1, nick: S2) -> anyhow::Result<()>
where
    S1: AsRef<str> + Display,
    S2: AsRef<str> + Display,
{
    info!("Giving +o on {channel} to {nick}");
    if let Err(e) = irc.send_mode(
        channel,
        &[Mode::Plus(ChannelMode::Oper, Some(nick.to_string()))],
    ) {
        error!("{e}");
        return Err(e.into());
    }
    // irc.send_privmsg(nick.as_ref(), "You got +v now.").ok();
    Ok(())
}

// EOF
