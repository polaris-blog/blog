use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;

use crate::error::{AppError, AppResult};

/// Standard five-field cron. UTC storage, IANA local-time evaluation.
/// Missing DST times are skipped. Repeated wall times run once, at the earlier
/// offset. Evaluate the calendar with Croner and resolve timezone ambiguity here
/// so an overlap can never return a timestamp before `after`.
pub fn next(expression: &str, timezone: &str, after: i64) -> AppResult<i64> {
    if expression.split_whitespace().count() != 5 || expression.len() > 256 {
        return Err(AppError::BadRequest("cron requires five fields".into()));
    }
    let tz: Tz = timezone
        .parse()
        .map_err(|_| AppError::BadRequest("invalid IANA timezone".into()))?;
    let schedule: Cron = expression
        .parse()
        .map_err(|_| AppError::BadRequest("invalid cron expression".into()))?;
    let after = DateTime::<Utc>::from_timestamp(after, 0)
        .ok_or_else(|| AppError::BadRequest("invalid timestamp".into()))?
        .with_timezone(&tz);
    let mut wall = after.naive_local().and_utc();
    for _ in 0..10000 {
        wall = schedule
            .find_next_occurrence(&wall, false)
            .map_err(|_| AppError::BadRequest("cron has no future occurrence".into()))?;
        if let Some(date) = tz.from_local_datetime(&wall.naive_utc()).earliest()
            && date.timestamp() > after.timestamp()
        {
            return Ok(date.timestamp());
        }
    }
    Err(AppError::BadRequest(
        "cron timezone search limit exceeded".into(),
    ))
}
