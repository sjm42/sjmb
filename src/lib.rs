// lib.rs

#![forbid(unsafe_code)]

pub use std::{
    cell::Cell,
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fmt::{self, Write as FmtWrite},
    fs::File,
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
};

pub use anyhow::{Context, anyhow, bail};
pub use chrono::{DateTime, Offset, Utc};
pub use chrono_tz::Tz;
pub use clap::Parser;
pub use irc::client::prelude::{Command, Config};
pub use regex::Regex;
pub use serde::{Deserialize, Serialize};
pub use tera::{Context as TeraContext, Kwargs, State, Tera};
pub use tokio::{
    net::lookup_host,
    select,
    signal::ctrl_c,
    sync::{Mutex, RwLock, mpsc},
    task::spawn_blocking,
    task_local,
    time::{Duration, Instant, interval, sleep, sleep_until, timeout},
};
pub use tracing::{Level, debug, error, info, trace, warn};
pub use tracing_subscriber::fmt as tracing_fmt;

#[cfg(test)]
pub use std::{env, fs, future::pending, io::Write as IoWrite, path::PathBuf, process, sync::atomic::AtomicBool};
#[cfg(test)]
pub use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    spawn,
    sync::Notify,
    task::JoinHandle,
};

pub use crate::{config::*, db_util::*, html_title::*, http_fetch::*, irc_case::*, ircbot::*, util::*};

pub mod config;
pub mod db_util;
pub mod html_title;
pub mod http_fetch;
pub mod irc_case;
pub mod ircbot;
pub mod util;

// EOF
