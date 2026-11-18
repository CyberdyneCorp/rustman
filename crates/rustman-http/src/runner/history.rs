//! Recording exchanges in history, and replaying recorded ones
//! (request-history spec).

use std::time::Instant;

use rustman_core::Redactor;
use rustman_core::history::{AD_HOC_REQUEST, Entry, NewEntry, ReplayRequest};
use rustman_core::model::REQUEST_FILE_SUFFIX;
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{RequestRecord, RequestStatus, SentRequest, SkipReason};
use rustman_core::vars::ResolvedRequest;

use super::execute::{AuthTarget, log_record, redact_error};
use super::record::exchange_record;
use super::{Runner, Sent};
use crate::exchange::Exchange;
use crate::timing::millis;

impl Runner<'_> {
    /// Record `sent` in history when history is on and the request was
    /// resolved, with everything it records redacted by `redactor` (see
    /// [`Runner::sink_redactor`]). `request` is its file (`None`: ad-hoc).
    /// The replay part keeps no value derived from a secret. Returns whether
    /// the entry was written and keeps the response's spill file. A failed write
    /// becomes a warning of the record, not a failure of the request.
    pub(super) fn record_history(
        &self,
        sent: &mut Sent,
        redactor: &Redactor,
        request: Option<String>,
        auth_cache_key: Option<String>,
    ) -> bool {
        let (Some(store), Some(resolved)) = (&self.options.history, &sent.resolved) else {
            return false;
        };
        let mut replay = ReplayRequest::new(resolved, auth_cache_key);
        let derived = self.lock_captures().derived().to_vec();
        replay.withhold_derived(&self.known(), &derived);
        let spill = sent
            .exchange
            .as_ref()
            .and_then(|e| e.spill.as_deref())
            .and_then(|path| store.spill_name(path));
        let entry = Entry::new(NewEntry {
            record: &sent.record,
            sent: SentRequest::new(resolved, redactor),
            request: request.as_deref(),
            environment: self.ctx.environment.map(|e| e.name.as_str()),
            spill,
            replay: Some(replay),
        });
        let keeps_spill = entry.spill.is_some();
        if let Err(e) = store.record(&entry) {
            let warning = format!("history: {e}");
            sent.record
                .warnings
                .push(redactor.text(&warning).into_owned());
            return false;
        }
        keeps_spill
    }

    /// Send `resolved` again, the request of history `entry`, and record it
    /// as a new entry. There are no assertions: a response passes.
    pub async fn replay(&self, entry: &Entry, mut resolved: ResolvedRequest) -> Sent {
        let started = Instant::now();
        let file = (entry.request != AD_HOC_REQUEST).then(|| entry.request.clone());
        let id = entry
            .request
            .strip_suffix(REQUEST_FILE_SUFFIX)
            .unwrap_or(&entry.request)
            .to_owned();
        let record = RequestRecord::new(id.clone(), id.clone(), entry.request.clone(), 1);
        let cache_key = entry.replay.as_ref().and_then(|r| r.auth_cache_key.clone());
        let target = AuthTarget {
            request: id,
            cache_key: cache_key.clone().unwrap_or_default(),
        };
        self.remember(&resolved.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved), None));
        let mut sent = match self.exchange(target, &mut resolved).await {
            Some(Ok(exchange)) => replayed(record, &resolved, exchange).with_resolved(resolved),
            Some(Err(e)) => Sent::errored(record, e.to_string()).with_resolved(resolved),
            None => Sent::skipped(record, SkipReason::Interrupted),
        };
        sent.record.duration_ms = millis(started.elapsed());
        let redactor = self.sink_redactor(&sent);
        redact_error(&mut sent.record, &redactor);
        sent.redactor = redactor.clone();
        let kept = self.record_history(&mut sent, &redactor, file, cache_key);
        sent.discard_spill_unless(kept);
        log_record(&sent.record);
        sent
    }
}

fn replayed(mut record: RequestRecord, resolved: &ResolvedRequest, exchange: Exchange) -> Sent {
    let redactor = exchange.redactor(resolved);
    remember_for_logs(&redactor);
    record.status = RequestStatus::Passed;
    record.warnings = exchange
        .warnings
        .iter()
        .map(|w| redactor.text(w).into_owned())
        .collect();
    record.exchange = Some(exchange_record(resolved, &exchange, &redactor));
    Sent {
        record,
        exchange: Some(exchange),
        resolved: None,
        discard: None,
        redactor: Redactor::new(),
    }
}
