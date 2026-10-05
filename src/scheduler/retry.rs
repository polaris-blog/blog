use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryStrategy {
    Fixed,
    #[default]
    Exponential,
}

pub fn delay(strategy: RetryStrategy, interval: u64, maximum: u64, retry_count: u32) -> u64 {
    let factor = match strategy {
        RetryStrategy::Fixed => 1,
        RetryStrategy::Exponential => 1u64 << retry_count.min(62),
    };
    interval.saturating_mul(factor).min(maximum)
}
