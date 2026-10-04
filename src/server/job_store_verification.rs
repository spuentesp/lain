//! Executable counterparts of `docs/formal/JobRegistry*.cfg`.
use super::*;
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use std::sync::{Arc, Barrier};

fn new_map() -> Arc<JobMap> {
    Arc::new(Mutex::new(HashMap::new()))
}

fn count(map: &JobMap, pred: impl Fn(&JobState) -> bool) -> usize {
    map.lock().values().filter(|j| pred(&j.state)).count()
}

/// CapBound under real contention: N threads race `try_start`; exactly `cap`
/// win, every round.
#[test]
fn concurrent_starts_never_exceed_the_cap() {
    const CAP: usize = 3;
    const THREADS: usize = 16;
    for round in 0..40 {
        let jobs = new_map();
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let (jobs, barrier) = (Arc::clone(&jobs), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    try_start(&jobs, CAP).is_ok()
                })
            })
            .collect();
        let won = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|w| *w)
            .count();
        assert_eq!(won, CAP, "round {round}: {won} jobs started with cap {CAP}");
        assert_eq!(count(&jobs, |s| matches!(s, JobState::Running)), CAP);
    }
}

#[test]
fn restored_running_jobs_become_interrupted_failures() {
    let jobs = new_map();
    let persisted = vec![
        JobInfo {
            id: "r".into(),
            created_at: SystemTime::now(),
            state: JobState::Running,
        },
        JobInfo {
            id: "d".into(),
            created_at: SystemTime::now(),
            state: JobState::Completed {
                success: true,
                output: Some("o".into()),
                error: None,
            },
        },
    ];
    restore(&jobs, persisted);
    assert_eq!(
        count(&jobs, |s| matches!(s, JobState::Running)),
        0,
        "ghost Running job"
    );
    match &jobs.lock()["r"].state {
        JobState::Completed {
            success: false,
            error: Some(e),
            ..
        } => {
            assert!(e.contains("interrupted"))
        }
        other => panic!("unexpected state {:?}", std::mem::discriminant(other)),
    }
    // A full set of crash-orphans must not lock out new work.
    let all_running: Vec<JobInfo> = (0..10)
        .map(|i| JobInfo {
            id: format!("g{i}"),
            created_at: SystemTime::now(),
            state: JobState::Running,
        })
        .collect();
    restore(&jobs, all_running);
    assert!(
        try_start(&jobs, 10).is_ok(),
        "orphaned jobs consumed the whole cap"
    );
}

#[test]
fn a_panicking_task_is_recorded_as_failed_not_left_running() {
    let jobs = new_map();
    let id = try_start(&jobs, 1).unwrap();
    // The task wrapper turns a panic into `Err(..)` and calls `finish`.
    finish(&jobs, &id, Err("background job panicked: boom".into()));
    assert_eq!(count(&jobs, |s| matches!(s, JobState::Running)), 0);
    assert!(try_start(&jobs, 1).is_ok(), "the slot must be free again");
}

#[test]
fn completed_jobs_are_bounded_oldest_first() {
    let jobs = new_map();
    let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    {
        let mut map = jobs.lock();
        for i in 0..(COMPLETED_JOB_RETENTION + 50) {
            map.insert(
                format!("j{i:05}"),
                JobInfo {
                    id: format!("j{i:05}"),
                    created_at: t0 + std::time::Duration::from_secs(i as u64),
                    state: JobState::Completed {
                        success: true,
                        output: None,
                        error: None,
                    },
                },
            );
        }
    }
    let id = try_start(&jobs, 10).unwrap();
    finish(&jobs, &id, Ok("x".into()));
    let map = jobs.lock();
    let completed = map
        .values()
        .filter(|j| matches!(j.state, JobState::Completed { .. }))
        .count();
    assert_eq!(completed, COMPLETED_JOB_RETENTION);
    assert!(!map.contains_key("j00000"), "the oldest must go first");
    assert!(map.contains_key(&id), "the newest must be kept");
}

// ---- state machine: the registry against a counter model ---------------------

#[derive(Clone, Debug)]
enum Op {
    Start,
    FinishOldest(bool), // ok / failed
    Restart,
}

#[derive(Clone, Debug, Default)]
struct Model {
    running: usize,
}

const CAP: usize = 4;

struct Ref;
impl ReferenceStateMachine for Ref {
    type State = Model;
    type Transition = Op;
    fn init_state() -> BoxedStrategy<Model> {
        Just(Model::default()).boxed()
    }
    fn transitions(m: &Model) -> BoxedStrategy<Op> {
        let mut ops = vec![Just(Op::Start).boxed(), Just(Op::Restart).boxed()];
        if m.running > 0 {
            ops.push(any::<bool>().prop_map(Op::FinishOldest).boxed());
        }
        proptest::strategy::Union::new(ops).boxed()
    }
    fn apply(mut m: Model, op: &Op) -> Model {
        match op {
            Op::Start => {
                if m.running < CAP {
                    m.running += 1;
                }
            }
            Op::FinishOldest(_) => m.running -= 1,
            Op::Restart => m.running = 0,
        }
        m
    }
}

struct Sut {
    jobs: Arc<JobMap>,
    live: Vec<String>,
}

impl StateMachineTest for Sut {
    type SystemUnderTest = Sut;
    type Reference = Ref;
    fn init_test(_: &Model) -> Sut {
        Sut {
            jobs: new_map(),
            live: vec![],
        }
    }
    fn apply(mut sut: Sut, _: &Model, op: Op) -> Sut {
        match op {
            Op::Start => {
                if let Ok(id) = try_start(&sut.jobs, CAP) {
                    sut.live.push(id);
                }
            }
            Op::FinishOldest(ok) => {
                let id = sut.live.remove(0);
                finish(
                    &sut.jobs,
                    &id,
                    if ok { Ok("o".into()) } else { Err("e".into()) },
                );
            }
            Op::Restart => {
                // Persist, drop every task, rebuild from the snapshot.
                let snapshot: Vec<JobInfo> = sut.jobs.lock().values().cloned().collect();
                sut.jobs = new_map();
                restore(&sut.jobs, snapshot);
                sut.live.clear();
            }
        }
        sut
    }
    fn check_invariants(sut: &Sut, m: &Model) {
        let running = count(&sut.jobs, |s| matches!(s, JobState::Running));
        assert_eq!(running, m.running, "running count refines the model");
        assert!(running <= CAP, "CapBound");
        assert_eq!(
            running,
            sut.live.len(),
            "NoGhost: every Running job has a task"
        );
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]
    #[test]
    fn registry_refines_the_cap_model(sequential 1..50 => Sut);
}
