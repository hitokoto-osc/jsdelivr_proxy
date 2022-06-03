use std::time::{SystemTime, SystemTimeError};

pub fn get_timestamp() -> Result<u128, SystemTimeError> {
    Ok(SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_millis())
}

pub fn must_get_timestamp() -> u128 {
    get_timestamp().unwrap()
}
