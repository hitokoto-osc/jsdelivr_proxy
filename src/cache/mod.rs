use super::CONFIG;
use deadpool_redis::{
    redis::{cmd, AsyncCommands, Client, FromRedisValue, RedisResult, ToRedisArgs},
    Config, Connection, CreatePoolError, Manager, Object, Pool, PoolError, Runtime,
};
use std::{error::Error, future::Future};

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

pub async fn remember<'a, T, F, Fut, RV>(
    key: T,
    func: F,
    ex: Option<usize>,
) -> Result<RV, Box<dyn Error>>
where
    T: ToRedisArgs + Send + Sync + 'a,
    F: FnOnce() -> Fut,
    Fut: Future<Output = RedisResult<RV>> + Send + 'a,
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
