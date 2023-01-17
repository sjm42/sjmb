// db_util.rs

use chrono::*;
use log::*;
use sqlx::{Connection, SqliteConnection};
use std::{thread, time};

const RETRY_CNT: usize = 5;
const RETRY_SLEEP: u64 = 1;

#[derive(Debug, sqlx::FromRow)]
pub struct DbUrl {
    pub id: i64,
    pub seen: i64,
    pub channel: String,
    pub nick: String,
    pub url: String,
}

#[derive(Debug)]
pub struct DbCtx {
    pub dbc: SqliteConnection,
    pub update_change: bool,
}

#[derive(Debug)]
pub struct UrlCtx {
    pub ts: i64,
    pub chan: String,
    pub nick: String,
    pub url: String,
}

pub async fn start_db<S>(db_file: S) -> anyhow::Result<DbCtx>
where
    S: AsRef<str>,
{
    let dbc = SqliteConnection::connect(&format!("sqlite:{}", db_file.as_ref())).await?;
    let db = DbCtx {
        dbc,
        update_change: true,
    };
    Ok(db)
}

const SQL_UPDATE_CHANGE: &str = "update url_changed set last=?";
pub async fn db_mark_change(dbc: &mut SqliteConnection) -> anyhow::Result<()> {
    sqlx::query(SQL_UPDATE_CHANGE)
        .bind(Utc::now().timestamp())
        .execute(dbc)
        .await?;
    Ok(())
}

const SQL_INSERT_URL: &str = "insert into url (id, seen, channel, nick, url) \
    values (null, ?, ?, ?, ?)";
pub async fn db_add_url(db: &mut DbCtx, ur: &UrlCtx) -> anyhow::Result<u64> {
    let mut rowcnt = 0;
    let mut retry = 0;
    while retry < RETRY_CNT {
        match sqlx::query(SQL_INSERT_URL)
            .bind(ur.ts)
            .bind(&ur.chan)
            .bind(&ur.nick)
            .bind(&ur.url)
            .execute(&mut db.dbc)
            .await
        {
            Ok(res) => {
                info!("Insert result: {res:#?}");
                retry = 0;
                rowcnt = res.rows_affected();
                break;
            }
            Err(e) => {
                error!("Insert failed: {e:?}");
            }
        }
        error!("Retrying in {}s...", RETRY_SLEEP);
        thread::sleep(time::Duration::new(RETRY_SLEEP, 0));
        retry += 1;
    }
    if db.update_change {
        db_mark_change(&mut db.dbc).await?;
    }
    if retry > 0 {
        error!("GAVE UP after {RETRY_CNT} retries.");
    }
    Ok(rowcnt)
}

// EOF
