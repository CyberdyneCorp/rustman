//! The one place captures enter the store: the filters decide what is
//! kept, after the exchange was proxied (spec: Capture filters).

use std::time::SystemTime;

use crate::capture::Recorded;
use crate::filter::CaptureFilter;
use crate::store::CaptureStore;

/// Records what the filter accepts.
#[derive(Clone)]
pub(crate) struct Recorder {
    pub store: CaptureStore,
    pub filter: CaptureFilter,
}

impl Recorder {
    /// Add `recorded` to the store unless the filter drops it.
    pub fn record(&self, started: SystemTime, recorded: Recorded) {
        if self.filter.accepts(&recorded) {
            self.store.push(started, recorded);
        }
    }
}
