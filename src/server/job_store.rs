//! Background-job registry operations (`ToolExecutor::call`, `background: true`).
//!
//! The operations are free functions over the shared map so the invariants of
//! `docs/formal/JobRegistry.tla` hold by construction and can be tested
//! directly:
//!
//! * **CapBound** — [`try_start`] counts and inserts in ONE critical section;
//!   counting under one lock acquisition and inserting under another let
//!   concurrent callers exceed the cap.
//! * **NoGhost**  — a `Running` entry always has a task behind it: the task
//!   wrapper records panics through [`finish`], and [`restore`] turns jobs
//!   persisted as `Running` into `interrupted` failures (nothing resumes them
//!   after a restart, so leaving them `Running` consumed cap slots forever).
//! * **Bounded**  — completed jobs are evicted oldest-first past
//!   [`COMPLETED_JOB_RETENTION`], so neither memory nor `jobs.json` grows
//!   without limit on a long-lived server.
use crate::server::tools::{JobInfo, JobState};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::SystemTime;

/// Completed jobs kept for `get_job_status` before the oldest are dropped.
pub(crate) const COMPLETED_JOB_RETENTION: usize = 1000;

pub(crate) type JobMap = Mutex<HashMap<String, JobInfo>>;

fn running(map: &HashMap<String, JobInfo>) -> usize {
    map.values()
        .filter(|j| matches!(j.state, JobState::Running))
        .count()
}

/// Register a new `Running` job unless `cap` jobs are already running.
/// Check and insert are atomic. Returns the new job id.
pub(crate) fn try_start(jobs: &JobMap, cap: usize) -> Result<String, usize> {
    let mut map = jobs.lock();
    let n = running(&map);
    if n >= cap {
        return Err(n);
    }
    let id = uuid::Uuid::new_v4().to_string();
    map.insert(
        id.clone(),
        JobInfo {
            id: id.clone(),
            created_at: SystemTime::now(),
            state: JobState::Running,
        },
    );
    Ok(id)
}

/// Record a job's outcome, then enforce the retention bound. A job that
/// panicked is a failure like any other (`Err`), never a stuck `Running`.
pub(crate) fn finish(jobs: &JobMap, id: &str, outcome: Result<String, String>) {
    let mut map = jobs.lock();
    if let Some(j) = map.get_mut(id) {
        j.state = match outcome {
            Ok(out) => JobState::Completed {
                success: true,
                output: Some(out),
                error: None,
            },
            Err(e) => JobState::Completed {
                success: false,
                output: None,
                error: Some(e),
            },
        };
    }
    evict_oldest_completed(&mut map, COMPLETED_JOB_RETENTION);
}

/// Load persisted jobs. Anything persisted as `Running` belongs to a process
/// that no longer exists: mark it failed instead of resurrecting a slot that
/// no task will ever release.
pub(crate) fn restore(jobs: &JobMap, persisted: Vec<JobInfo>) {
    let mut map = jobs.lock();
    for mut j in persisted {
        if matches!(j.state, JobState::Running) {
            j.state = JobState::Completed {
                success: false,
                output: None,
                error: Some("interrupted: the server restarted while this job was running".into()),
            };
        }
        map.insert(j.id.clone(), j);
    }
    evict_oldest_completed(&mut map, COMPLETED_JOB_RETENTION);
}

fn evict_oldest_completed(map: &mut HashMap<String, JobInfo>, keep: usize) {
    let mut done: Vec<(SystemTime, String)> = map
        .values()
        .filter(|j| matches!(j.state, JobState::Completed { .. }))
        .map(|j| (j.created_at, j.id.clone()))
        .collect();
    let excess = done.len().saturating_sub(keep);
    if excess == 0 {
        return;
    }
    done.sort();
    for (_, id) in done.into_iter().take(excess) {
        map.remove(&id);
    }
}

#[cfg(test)]
#[path = "job_store_verification.rs"]
mod verification;
