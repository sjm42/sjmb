// db_util.rs

use anyhow::Context;
use futures::TryStreamExt;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

use crate::*;

const RETRY_CNT: usize = 5;
const RETRY_SLEEP: u64 = 1;
const DB_MAX_CONNECTIONS: u32 = 4;

#[derive(Debug, sqlx::FromRow)]
pub struct DbUrl {
    pub id: i64,
    pub seen: i64,
    pub channel: String,
    pub nick: String,
    pub url: String,
}

#[derive(Clone)]
pub struct DbCtx {
    pub dbc: Pool<Postgres>,
}

impl std::fmt::Debug for DbCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbCtx").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct UrlCtx {
    pub ts: i64,
    pub chan: String,
    pub nick: String,
    pub url: String,
}

pub async fn start_db<S>(db_url: S) -> anyhow::Result<DbCtx>
where
    S: AsRef<str>,
{
    debug!("start_db(): creating pool");
    let dbc = PgPoolOptions::new()
        .max_connections(DB_MAX_CONNECTIONS)
        .connect(db_url.as_ref())
        .await?;
    let db = DbCtx { dbc };
    debug!("start_db(): pool created");
    Ok(db)
}

const SQL_INSERT_URL: &str = "insert into url (seen, channel, nick, url) \
    values ($1, $2, $3, $4)";

pub async fn db_add_url(db: &DbCtx, ur: &UrlCtx) -> anyhow::Result<u64> {
    debug!("db_add_url({ur:?})");
    for attempt in 1..=RETRY_CNT {
        match db_add_url_once(db, ur).await {
            Ok(rowcnt) => {
                info!("db_add_url: Ok({rowcnt})");
                return Ok(rowcnt);
            }
            Err(e) if attempt == RETRY_CNT => {
                return Err(e)
                    .with_context(|| format!("URL insert failed after {RETRY_CNT} attempts for channel {}", ur.chan));
            }
            Err(e) => {
                warn!(
                    "URL insert attempt {attempt}/{RETRY_CNT} failed for channel {}: {e:#}",
                    ur.chan
                );
                sleep(Duration::new(RETRY_SLEEP, 0)).await;
            }
        }
    }

    unreachable!("retry loop always returns");
}

async fn db_add_url_once(db: &DbCtx, ur: &UrlCtx) -> anyhow::Result<u64> {
    let res = sqlx::query(SQL_INSERT_URL)
        .bind(ur.ts)
        .bind(&ur.chan)
        .bind(&ur.nick)
        .bind(&ur.url)
        .execute(&db.dbc)
        .await?;

    Ok(res.rows_affected())
}

#[derive(Debug, sqlx::FromRow)]
pub struct CheckUrl {
    pub cnt: i64,
    pub first: Option<i64>,
    pub last: Option<i64>,
}

const SQL_CHECK_URL: &str = "select count(id) as cnt, min(seen) as first, max(seen) as last \
     from url \
     where url = $1 and channel = $2 and seen > $3";

pub async fn db_check_url(db: &DbCtx, url: &str, chan: &str, expire_s: i64) -> anyhow::Result<Option<CheckUrl>> {
    debug!("db_check_url(): url {url}");
    let mut st_check_url = sqlx::query_as::<_, CheckUrl>(SQL_CHECK_URL)
        .bind(url)
        .bind(chan)
        .bind(Utc::now().timestamp() - expire_s)
        .fetch(&db.dbc);
    let res = st_check_url.try_next().await?;

    info!("db_check_url: {res:?}");
    Ok(res)
}
// EOF
