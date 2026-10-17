// db_util.rs

use sqlx::{
    Pool, Postgres,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::*;

const DB_JOB_TIMEOUT: Duration = Duration::from_secs(8);
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

impl fmt::Debug for DbCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
    let options = db_url
        .as_ref()
        .parse::<PgConnectOptions>()?
        .options([("statement_timeout", "5000"), ("lock_timeout", "1000")]);
    let dbc = PgPoolOptions::new()
        .max_connections(DB_MAX_CONNECTIONS)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy_with(options);
    let db = DbCtx { dbc };
    debug!("start_db(): pool created");
    Ok(db)
}

const SQL_INSERT_URL: &str = "insert into url (seen, channel, nick, url) \
    values ($1, $2, $3, $4)";

pub async fn db_add_url(db: &DbCtx, ur: &UrlCtx) -> anyhow::Result<u64> {
    debug!("db_add_url({ur:?})");
    timeout(DB_JOB_TIMEOUT, db_add_url_once(db, ur))
        .await
        .context("URL insert timed out")?
        .with_context(|| format!("URL insert failed for channel {}", ur.chan))
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
    let since = Utc::now()
        .timestamp()
        .checked_sub(expire_s)
        .context("URL expiry timestamp overflow")?;
    let res = timeout(
        DB_JOB_TIMEOUT,
        sqlx::query_as::<_, CheckUrl>(SQL_CHECK_URL)
            .bind(url)
            .bind(chan)
            .bind(since)
            .fetch_optional(&db.dbc),
    )
    .await
    .context("URL duplicate check timed out")??;

    info!("db_check_url: {res:?}");
    Ok(res)
}
// EOF
