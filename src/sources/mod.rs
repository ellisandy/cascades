pub mod noaa;
pub mod road_closures;
pub mod trail_conditions;
pub mod usgs;
pub mod wsdot;

use crate::domain::DataPoint;
use std::time::Duration;
use thiserror::Error;

/// Error returned by a source's fetch() call.
#[derive(Debug, Error)]
pub enum SourceError {
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("source error: {0}")]
    Other(String),
}

/// Every data source implements this trait.
///
/// Sources run on independent threads. Each call to `fetch` is blocking; the
/// scheduler calls it on a thread dedicated to that source. On error, the
/// source should log and return `Err`; the scheduler will retry after
/// `refresh_interval`. Sources must not panic.
pub trait Source: Send {
    /// Human-readable name shown in the web UI and logs.
    fn name(&self) -> &str;

    /// How often the scheduler should call `fetch`.
    fn refresh_interval(&self) -> Duration;

    /// Fetch the latest data. Returns a DataPoint on success or a SourceError
    /// on failure. Never panics. Never blocks indefinitely — callers rely on
    /// this to drain the scheduler.
    fn fetch(&self) -> Result<DataPoint, SourceError>;
}
