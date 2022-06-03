use crate::CONFIG;
use deadpool_redis::{
    redis::{AsyncCommands, FromRedisValue, RedisError, ToRedisArgs},
    Config, Connection, CreatePoolError, Pool, PoolError, Runtime,
};
use std::{
    error,
    fmt::{self, Debug, Formatter},
    future::Future,
};

lazy_static! {
    static ref CACHE: Cache = Cache::init().expect("Failed to initialize cache");
}

struct Cache {
    pool: Pool,
}

impl Cache {
    pub fn init() -> Result<Self, CreatePoolError> {
        let cfg = Config::from_url(&(*CONFIG).redis.to_uri());
        let pool = cfg.create_pool(Some(Runtime::Tokio1))?;
        Ok(Cache { pool })
    }

    #[allow(dead_code)]
    pub fn get_pool(&self) -> &Pool {
        &self.pool
    }

    pub async fn get_connection(&self) -> Result<Connection, PoolError> {
        self.pool.get().await
    }
}

pub async fn get_connection() -> Result<Connection, PoolError> {
    (*CACHE).get_connection().await
}

// 回调闭包错误
#[derive(Debug)]
pub struct RememberFuncCallError<T: error::Error>(pub T);

impl<E: error::Error> fmt::Display for RememberFuncCallError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<E: error::Error + 'static> error::Error for RememberFuncCallError<E> {
    fn source(&self) -> Option<&(dyn error::Error + 'static)>{
        Some(&self.0)
    }
}

impl<E: error::Error> From<E> for RememberFuncCallError<E> {
    fn from(e: E) -> Self {
        RememberFuncCallError(e)
    }
}


// 缓存内部错误
#[derive(Debug)]
pub enum CacheError<T: error::Error> {
    Pool(PoolError),
    Redis(RedisError),
    RememberFuncCall(RememberFuncCallError<T>),
}

impl <E: error::Error> From<RedisError> for CacheError<E> {
    fn from(e: RedisError) -> Self {
        CacheError::Redis(e)
    }
}

impl <E: error::Error> From<PoolError> for CacheError<E> {
    fn from(e: PoolError) -> Self {
        CacheError::Pool(e)
    }
}

impl <E: error::Error> From<RememberFuncCallError<E>> for CacheError<E> {
    fn from(e: RememberFuncCallError<E>) -> Self {
        CacheError::RememberFuncCall(e)
    }
}


pub async fn remember<'a, T, E, F, Fut, RV>(
    key: T,
    func: F,
    ex: Option<usize>,
) -> Result<RV, CacheError<E>>
where
    T: ToRedisArgs + Send + Sync + 'a,
    E: error::Error,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<RV, RememberFuncCallError<E>>> + Send + 'a,
    RV: FromRedisValue + ToRedisArgs + Send + Sync + 'a,
{
    let mut conn = get_connection().await?;
    let cached: Option<RV> = conn.get(&key).await?;
    if let Some(cached) = cached {
        return Ok(cached);
    }
    let result = func().await?;
    match ex {
        Some(ex) => conn.set_ex(&key, &result, ex).await?,
        None => conn.set(&key, &result).await?,
    }
    Ok(result)
}
