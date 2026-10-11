//! Syntax highlighting and folding off the frame path.
//!
//! Parsing a cold Tree-sitter window costs milliseconds to tens of
//! milliseconds; done synchronously it stalls scrolling. Each surface owns one
//! worker thread instead. The surface sends edits (to keep the incremental
//! parse current) and analysis requests for the rows around the viewport; the
//! worker answers with tokens and fold ranges, and the surface applies them on
//! the next frame. Frames never wait.
//!
//! The worker processes edits strictly in order, but of the analysis requests
//! queued behind one another only the newest current one runs: while a
//! scrollbar is dragged through a huge file, the windows the viewport passed
//! by on the way are never parsed.

use mockaco_core::{AppliedTransaction, ChangeMap, DocumentSnapshot, Transaction};
use mockaco_language::{
    FoldRange, FoldingProvider, GenericSyntaxProvider, HighlightResult, RustTreeSitterProvider,
    SyntaxHighlightProvider,
};
use std::fmt;
use std::ops::Range;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum Provider {
    Rust(Arc<RustTreeSitterProvider>),
    Generic(Arc<GenericSyntaxProvider>),
}

impl Provider {
    fn analyze(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> (Option<HighlightResult>, Option<Vec<FoldRange>>) {
        match self {
            Self::Rust(provider) => (
                provider.highlight(snapshot, range.clone()).ok(),
                provider.fold_ranges(snapshot, range).ok(),
            ),
            Self::Generic(provider) => (
                provider.highlight(snapshot, range.clone()).ok(),
                provider.fold_ranges(snapshot, range).ok(),
            ),
        }
    }

    fn edit(&self, before: &DocumentSnapshot, after: &DocumentSnapshot, changes: &ChangeMap) {
        if let Self::Rust(provider) = self {
            // Only the versions and the change map are read; the full text
            // is never copied across the thread.
            let applied = AppliedTransaction {
                before_version: before.version(),
                after_version: after.version(),
                text: Default::default(),
                change_map: changes.clone(),
                inverse: Transaction::new(),
            };
            let range = 0..0;
            let _ = provider.apply_transaction(before, after, &applied, range);
        }
    }
}

enum Job {
    Edit {
        before: DocumentSnapshot,
        after: DocumentSnapshot,
        changes: ChangeMap,
    },
    Analyze {
        snapshot: DocumentSnapshot,
        range: Range<usize>,
    },
}

/// What the worker produced for one job.
pub(crate) enum Outcome {
    /// An edit was folded into the parser state, or a superseded request was
    /// dropped. Counted so the surface knows when the worker is idle.
    Done,
    Analyzed {
        version: u64,
        range: Range<usize>,
        highlights: Option<HighlightResult>,
        folds: Option<Vec<FoldRange>>,
    },
}

pub(crate) struct LanguageWorker {
    jobs: Sender<Job>,
    outcomes: Receiver<Outcome>,
    in_flight: usize,
}

impl fmt::Debug for LanguageWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanguageWorker")
            .field("in_flight", &self.in_flight)
            .finish_non_exhaustive()
    }
}

impl LanguageWorker {
    pub(crate) fn spawn(provider: Provider) -> Self {
        let (jobs, job_rx) = channel::<Job>();
        let (outcome_tx, outcomes) = channel::<Outcome>();
        std::thread::Builder::new()
            .name("mockaco-language".into())
            .spawn(move || run(provider, job_rx, outcome_tx))
            .expect("spawn the language worker");
        Self {
            jobs,
            outcomes,
            in_flight: 0,
        }
    }

    pub(crate) fn edit(
        &mut self,
        before: DocumentSnapshot,
        after: DocumentSnapshot,
        changes: ChangeMap,
    ) {
        if self
            .jobs
            .send(Job::Edit {
                before,
                after,
                changes,
            })
            .is_ok()
        {
            self.in_flight += 1;
        }
    }

    pub(crate) fn analyze(&mut self, snapshot: DocumentSnapshot, range: Range<usize>) {
        if self.jobs.send(Job::Analyze { snapshot, range }).is_ok() {
            self.in_flight += 1;
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.in_flight > 0
    }

    /// The next finished job, without waiting.
    pub(crate) fn try_next(&mut self) -> Option<Outcome> {
        let outcome = self.outcomes.try_recv().ok()?;
        self.in_flight = self.in_flight.saturating_sub(1);
        Some(outcome)
    }

    /// The next finished job, waiting for it. `None` once nothing is in
    /// flight (or the worker is gone).
    pub(crate) fn next_blocking(&mut self) -> Option<Outcome> {
        if self.in_flight == 0 {
            return None;
        }
        match self.outcomes.recv() {
            Ok(outcome) => {
                self.in_flight -= 1;
                Some(outcome)
            }
            Err(_) => {
                self.in_flight = 0;
                None
            }
        }
    }
}

fn run(provider: Provider, jobs: Receiver<Job>, outcomes: Sender<Outcome>) {
    while let Ok(first) = jobs.recv() {
        let mut batch = vec![first];
        batch.extend(jobs.try_iter());
        // Only the last analysis of the newest version is worth doing.
        let newest = batch
            .iter()
            .map(|job| match job {
                Job::Edit { after, .. } => after.version(),
                Job::Analyze { snapshot, .. } => snapshot.version(),
            })
            .max()
            .unwrap_or(0);
        let last_analysis = batch.iter().rposition(|job| {
            matches!(job, Job::Analyze { snapshot, .. } if snapshot.version() == newest)
        });
        for (index, job) in batch.into_iter().enumerate() {
            let outcome = match job {
                Job::Edit {
                    before,
                    after,
                    changes,
                } => {
                    provider.edit(&before, &after, &changes);
                    Outcome::Done
                }
                Job::Analyze { snapshot, range } if Some(index) == last_analysis => {
                    let (highlights, folds) = provider.analyze(&snapshot, range.clone());
                    Outcome::Analyzed {
                        version: snapshot.version(),
                        range,
                        highlights,
                        folds,
                    }
                }
                Job::Analyze { .. } => Outcome::Done,
            };
            if outcomes.send(outcome).is_err() {
                return;
            }
        }
    }
}
