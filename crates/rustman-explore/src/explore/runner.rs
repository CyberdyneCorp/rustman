//! Executing a plan (design D4): the probes not yet in the journal run at
//! most `concurrency` at a time through one [`Executor`], each outcome is
//! journaled as it finishes, and the resources they created are cleaned up
//! at the end. A cancellation (Ctrl+C) stops dispatching, waits up to the
//! drain time for the probes in flight, and still cleans up; the probes
//! that did not finish run on resume. What a probe still in flight when
//! the drain time ran out had created is journaled with it as
//! interrupted, so it is cleaned up (or listed as left behind).

use std::collections::{BTreeSet, HashSet};
use std::io;
use std::pin::pin;
use std::time::{Duration, Instant};

use futures_util::future::{Either, select};
use futures_util::stream::{self, StreamExt};
use tokio_util::sync::CancellationToken;

use super::exec::{Executor, Outcome, Status};
use super::journal::{Cleanup, Entry, Journal, outcomes};
use super::plan::{Plan, Probe};
use super::resources::Resource;
use super::rules::Severity;
use super::session::Progress;

/// How long probes in flight may finish after a Ctrl+C.
pub const DRAIN: Duration = Duration::from_secs(10);

/// How to execute.
pub struct RunOptions<'a> {
    /// Probes in flight at once.
    pub concurrency: usize,
    /// Cancelled on Ctrl+C.
    pub cancel: CancellationToken,
    /// How long probes in flight may finish after a cancellation.
    pub drain: Duration,
    /// Called with the progress before the first probe and after each one.
    pub progress: &'a mut dyn FnMut(&Progress),
    /// Called with each probe's outcome (redacted, as journaled) and how
    /// long it ran, as it finishes (`explore run --viz`).
    pub probe: Option<&'a mut ProbeObserver<'a>>,
}

/// See [`RunOptions::probe`].
pub type ProbeObserver<'a> = dyn FnMut(&Outcome, Duration) + 'a;

/// What execution left in the journal.
#[derive(Debug)]
pub struct Executed {
    /// Cancelled before every probe finished.
    pub interrupted: bool,
    /// Every journal entry, earlier runs included.
    pub entries: Vec<Entry>,
}

/// Run the probes of `plan` that `entries` (the journal so far) does not
/// hold, appending to `journal`, then clean up.
pub async fn execute(
    executor: &Executor<'_>,
    plan: &Plan,
    journal: &mut Journal,
    entries: Vec<Entry>,
    options: RunOptions<'_>,
) -> io::Result<Executed> {
    let done: HashSet<&str> = outcomes(&entries).map(|o| o.probe.as_str()).collect();
    let pending: Vec<&Probe> = plan
        .probes
        .iter()
        .filter(|p| !done.contains(p.id.as_str()))
        .collect();
    let mut run = Run {
        journal,
        tally: Tally::new(plan.probes.len(), &entries),
        entries,
        progress: options.progress,
        probe: options.probe,
    };
    (run.progress)(&run.tally.progress);
    let finished = run
        .probes(
            executor,
            &pending,
            options.concurrency,
            &options.cancel,
            options.drain,
        )
        .await?;
    run.cleanup(executor).await?;
    Ok(Executed {
        interrupted: finished < pending.len(),
        entries: run.entries,
    })
}

struct Run<'j, 'p> {
    journal: &'j mut Journal,
    entries: Vec<Entry>,
    tally: Tally,
    progress: &'p mut dyn FnMut(&Progress),
    probe: Option<&'p mut ProbeObserver<'p>>,
}

impl Run<'_, '_> {
    /// Run `pending` until done or cancelled; how many finished.
    async fn probes(
        &mut self,
        executor: &Executor<'_>,
        pending: &[&Probe],
        concurrency: usize,
        cancel: &CancellationToken,
        drain: Duration,
    ) -> io::Result<usize> {
        let mut outcomes = pin!(
            stream::iter(pending)
                .take_until(cancel.clone().cancelled_owned())
                .map(|probe| async move {
                    let started = Instant::now();
                    let outcome = executor.run(probe).await;
                    (outcome, started.elapsed())
                })
                .buffer_unordered(concurrency.max(1))
        );
        let mut deadline = pin!(async {
            cancel.cancelled().await;
            tokio::time::sleep(drain).await;
        });
        let mut finished = 0;
        while let Either::Left((Some((outcome, took)), _)) =
            select(outcomes.next(), deadline.as_mut()).await
        {
            if let Some(probe) = self.probe.as_mut() {
                probe(&outcome, took);
            }
            self.record(outcome)?;
            finished += 1;
        }
        for (probe, created) in executor.abandoned() {
            let mut outcome = Outcome::new(&probe, Status::interrupted());
            outcome.created = created;
            let entry = Entry::Outcome(outcome);
            self.journal.append(&entry)?;
            self.entries.push(entry);
        }
        Ok(finished)
    }

    fn record(&mut self, outcome: Outcome) -> io::Result<()> {
        self.tally.add(&outcome);
        let entry = Entry::Outcome(outcome);
        self.journal.append(&entry)?;
        self.entries.push(entry);
        (self.progress)(&self.tally.progress);
        Ok(())
    }

    /// Delete what the probes left, newest first, when the policy allows
    /// DELETE; otherwise the report lists it as left behind.
    async fn cleanup(&mut self, executor: &Executor<'_>) -> io::Result<()> {
        if executor.sender().policy().check_method("DELETE").is_err() {
            return Ok(());
        }
        let deletable = live(&self.entries)
            .into_iter()
            .filter(|r| r.delete.is_some());
        for resource in deletable.rev() {
            let result = executor.delete(&resource).await;
            let entry = Entry::Cleanup(Cleanup {
                path: resource.path,
                url: resource.url,
                status: result.as_ref().ok().copied(),
                error: result.err().map(|status| status.to_string()),
            });
            self.journal.append(&entry)?;
            self.entries.push(entry);
        }
        Ok(())
    }
}

/// The resources created and not deleted since, in creation order.
pub fn live(entries: &[Entry]) -> Vec<Resource> {
    let mut resources: Vec<Resource> = Vec::new();
    for entry in entries {
        match entry {
            Entry::Outcome(o) => {
                resources.extend(o.created.iter().cloned());
                resources.retain(|r| !o.deleted.contains(&r.path));
            }
            Entry::Cleanup(c) if c.deleted() => resources.retain(|r| r.path != c.path),
            Entry::Cleanup(_) => {}
        }
    }
    resources
}

/// Progress, with each finding counted once per rule and operation.
struct Tally {
    progress: Progress,
    seen: BTreeSet<(String, String)>,
}

impl Tally {
    fn new(total: usize, entries: &[Entry]) -> Self {
        let mut tally = Self {
            progress: Progress {
                total,
                ..Progress::default()
            },
            seen: BTreeSet::new(),
        };
        for outcome in outcomes(entries) {
            tally.add(outcome);
        }
        tally
    }

    fn add(&mut self, outcome: &Outcome) {
        self.progress.done += 1;
        for f in &outcome.findings {
            if self.seen.insert((f.rule.clone(), f.operation.clone())) {
                self.progress.findings.add(f.severity);
            }
        }
    }
}

/// Distinct findings at or above `threshold` in `entries`.
pub fn findings_at_or_above(entries: &[Entry], threshold: Severity) -> usize {
    Tally::new(0, entries)
        .progress
        .findings
        .at_or_above(threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(path: &str) -> Resource {
        Resource {
            probe: "p".into(),
            operation: "POST /users".into(),
            path: path.into(),
            url: format!("http://h{path}"),
            delete: Some("DELETE /users/{id}".into()),
        }
    }

    #[test]
    fn deleted_and_cleaned_resources_are_not_live() {
        let mut created = Outcome::new("p1", Status::Done);
        created.created = vec![resource("/users/2"), resource("/users/3")];
        let mut chain = Outcome::new("p2", Status::Done);
        chain.created = vec![resource("/users/4")];
        chain.deleted = vec!["/users/4".into()];
        let cleanup = |path: &str, status| {
            Entry::Cleanup(Cleanup {
                path: path.into(),
                url: String::new(),
                status: Some(status),
                error: None,
            })
        };
        let mut cut = Outcome::new("p3", Status::interrupted());
        cut.created = vec![resource("/users/5")];
        let entries = vec![
            Entry::Outcome(created),
            Entry::Outcome(chain),
            Entry::Outcome(cut),
            cleanup("/users/5", 204),
            cleanup("/users/3", 204),
            cleanup("/users/2", 500),
        ];
        let paths: Vec<String> = live(&entries).into_iter().map(|r| r.path).collect();
        assert_eq!(paths, ["/users/2"]);
    }
}
