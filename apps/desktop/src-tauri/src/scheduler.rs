//! Runs scheduled snippets while Vessel is open (also from the tray).
//!
//! Only snippets of the personal vault with a target are scheduled, and only while the vault is
//! unlocked. A time that comes while the vault is locked is recorded as skipped; a time that
//! passed while Vessel was closed (or the computer slept) is recorded as missed and does not run
//! late. Several missed times of one schedule become one history entry.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use serde::Serialize;
use tauri::Emitter;
use uuid::Uuid;
use vessel_client::model::{Entity, EntityKind, SnippetTarget};

use crate::cron::Cron;
use crate::runs::{self, RunOrigin, RunRecord, RunStatus};
use crate::state::{AppState, now};

const TICK: Duration = Duration::from_secs(15);
/// A time is run if the scheduler sees it at most this late.
const GRACE_SECS: i64 = 90;
const LAST_KEY: &str = "scheduler-last";

/// What the scheduler observed, kept in memory.
pub struct SchedulerState {
    started_at: i64,
    /// Periods (start, end) when the app was open with the vault locked. A period ends at the
    /// first tick that finds the vault unlocked again.
    locked: Mutex<Vec<(i64, i64)>>,
    was_locked: std::sync::atomic::AtomicBool,
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self {
            started_at: now(),
            locked: Mutex::new(Vec::new()),
            was_locked: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ScheduleInfo {
    pub snippet_id: Uuid,
    pub index: usize,
    pub cron: String,
    pub enabled: bool,
    pub next: Option<i64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchedulerStatus {
    pub paused: bool,
    pub running: Vec<Uuid>,
    pub schedules: Vec<ScheduleInfo>,
}

struct Due {
    snippet_id: Uuid,
    label: String,
    cron: Cron,
    /// When this schedule was first seen: times before it never count as missed.
    since: i64,
}

/// First time each schedule was seen (by snippet and expression), sealed in the vault: a schedule
/// created now must not report the times before it existed as missed.
const SEEN_KEY: &str = "scheduler-seen";

fn local(ts: i64) -> DateTime<Local> {
    Local
        .timestamp_opt(ts, 0)
        .earliest()
        .unwrap_or_else(Local::now)
}

/// Every schedule of the personal vault's snippets, with its next time.
pub async fn status(state: &AppState) -> SchedulerStatus {
    let mut schedules = Vec::new();
    let vessel = state.vessel.lock().await;
    if let (Ok(personal), Ok(items)) = (
        vessel.personal_vault(),
        vessel.items(Some(EntityKind::Snippet)),
    ) {
        let now = Local::now();
        let aws = crate::aws::snippets::targets(&vessel);
        for item in items.iter().filter(|i| i.vault_id == personal) {
            let Entity::Snippet(s) = &item.entity else {
                continue;
            };
            for (index, sch) in s.schedules.iter().enumerate() {
                let parsed = Cron::parse(&sch.cron);
                schedules.push(ScheduleInfo {
                    snippet_id: item.id,
                    index,
                    cron: sch.cron.clone(),
                    enabled: sch.enabled,
                    next: parsed
                        .as_ref()
                        .ok()
                        .filter(|_| sch.enabled && (s.target != SnippetTarget::None || aws.contains_key(&item.id)))
                        .and_then(|c| c.next_local(now))
                        .map(|d| d.timestamp()),
                    error: parsed.err(),
                });
            }
        }
    }
    drop(vessel);
    SchedulerStatus {
        paused: state.settings().schedules_paused,
        running: state.runs.running(),
        schedules,
    }
}

pub fn spawn(state: AppState) {
    tauri::async_runtime::spawn(async move {
        loop {
            tick(&state).await;
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(state: &AppState) {
    let now_ts = now();
    let (due, last) = {
        let vessel = state.vessel.lock().await;
        if !vessel.status().unlocked {
            let mut locked = state
                .scheduler
                .locked
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match locked.last_mut() {
                Some((_, end)) if now_ts - *end <= 2 * TICK.as_secs() as i64 => *end = now_ts,
                _ => locked.push((now_ts, now_ts)),
            }
            state.scheduler.was_locked.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if state.scheduler.was_locked.swap(false, std::sync::atomic::Ordering::Relaxed)
            && let Some((_, end)) = state.scheduler.locked.lock().unwrap_or_else(|e| e.into_inner()).last_mut()
        {
            // Unlocked somewhere since the last tick: count that whole gap as locked.
            *end = now_ts;
        }
        let Ok(personal) = vessel.personal_vault() else {
            return;
        };
        let Ok(items) = vessel.items(Some(EntityKind::Snippet)) else {
            return;
        };
        let aws = crate::aws::snippets::targets(&vessel);
        let due = items
            .iter()
            .filter(|i| i.vault_id == personal)
            .filter_map(|i| match &i.entity {
                Entity::Snippet(s) if s.target != SnippetTarget::None || aws.contains_key(&i.id) => Some((i.id, s)),
                _ => None,
            })
            .flat_map(|(id, s)| {
                s.schedules
                    .iter()
                    .filter(|x| x.enabled)
                    .filter_map(move |x| {
                        Some(Due {
                            snippet_id: id,
                            label: s.label.clone(),
                            cron: Cron::parse(&x.cron).ok()?,
                            since: 0,
                        })
                        .map(|d| (format!("{id}|{}", x.cron.trim()), d))
                    })
            })
            .collect::<Vec<_>>();
        let mut seen: std::collections::BTreeMap<String, i64> =
            vessel.device_sealed(SEEN_KEY).ok().flatten().unwrap_or_default();
        seen.retain(|k, _| due.iter().any(|(key, _)| key == k));
        let due: Vec<Due> = due
            .into_iter()
            .map(|(key, mut d)| {
                d.since = *seen.entry(key).or_insert(now_ts);
                d
            })
            .collect();
        let _ = vessel.set_device_sealed(SEEN_KEY, &seen);
        let last: i64 = vessel
            .device_plain(LAST_KEY)
            .ok()
            .flatten()
            .unwrap_or(now_ts);
        let _ = vessel.set_device_plain(LAST_KEY, &now_ts);
        (due, last)
    };
    // Records kept while locked are saved now that the vault is open.
    runs::flush(state).await;
    if last >= now_ts {
        return;
    }
    let paused = state.settings().schedules_paused;
    let locked = state
        .scheduler
        .locked
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut started = std::collections::HashSet::new();
    for action in plan(
        &due,
        last,
        now_ts,
        paused,
        state.scheduler.started_at,
        &locked,
    ) {
        match action {
            Action::Late {
                index,
                status,
                times,
                why,
            } => {
                let d = &due[index];
                let (first, last) = (times[0], times[times.len() - 1]);
                let note = if times.len() == 1 {
                    format!("Horário de {} não rodou: {why}.", fmt(first))
                } else {
                    format!(
                        "{} horários não rodaram, de {} a {}: {why}.",
                        times.len(),
                        fmt(first),
                        fmt(last)
                    )
                };
                record(state, d, first, status, note).await;
            }
            Action::Run { index, at } => {
                let d = &due[index];
                if !started.insert(d.snippet_id) {
                    continue;
                }
                if state.runs.is_running(&d.snippet_id) {
                    record(
                        state,
                        d,
                        at,
                        RunStatus::Skipped,
                        "Não rodou: a execução anterior ainda estava rodando.".into(),
                    )
                    .await;
                    continue;
                }
                let (state, id, label) = (state.clone(), d.snippet_id, d.label.clone());
                tauri::async_runtime::spawn(async move {
                    match runs::run(&state, id, RunOrigin::Scheduled, false).await {
                        Ok(r) if r.status == RunStatus::Failed => {
                            notify_failure(&state, &label, &r)
                        }
                        Ok(_) => {}
                        Err(e) => notify(&state, &format!("\"{label}\" não rodou"), &e.message),
                    }
                });
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Action {
    /// Run schedule `index` now (its time `at` just came).
    Run { index: usize, at: i64 },
    /// Times of schedule `index` that passed without running, and why.
    Late {
        index: usize,
        status: RunStatus,
        times: Vec<i64>,
        why: &'static str,
    },
}

/// What to do for the times in `(last, now]`. A time seen within the grace period runs; older
/// ones are reported: before the app started (closed), while the vault was locked (skipped), or
/// otherwise (the computer slept). Nothing happens while paused.
fn plan(
    due: &[Due],
    last: i64,
    now: i64,
    paused: bool,
    started_at: i64,
    locked: &[(i64, i64)],
) -> Vec<Action> {
    let mut out = Vec::new();
    if paused || last >= now {
        return out;
    }
    let slack = TICK.as_secs() as i64;
    for (index, d) in due.iter().enumerate() {
        let from = last.max(d.since);
        if from >= now {
            continue;
        }
        let times: Vec<i64> = d
            .cron
            .between(local(from), local(now), 10_000)
            .iter()
            .map(DateTime::timestamp)
            .collect();
        let Some(&latest) = times.last() else {
            continue;
        };
        let in_lock = |t: i64| locked.iter().any(|(a, b)| t >= a - slack && t <= *b);
        // A time that came while the vault was locked is skipped, even if the vault opened
        // within the grace period.
        let on_time = now - latest <= GRACE_SECS && latest >= started_at && !in_lock(latest);
        let (mut closed, mut was_locked, mut other) = (Vec::new(), Vec::new(), Vec::new());
        for &t in times.iter().filter(|t| !(on_time && **t == latest)) {
            if t < started_at {
                closed.push(t);
            } else if in_lock(t) {
                was_locked.push(t);
            } else {
                other.push(t);
            }
        }
        for (times, status, why) in [
            (closed, RunStatus::Missed, "o Vessel estava fechado"),
            (was_locked, RunStatus::Skipped, "o cofre estava bloqueado"),
            (
                other,
                RunStatus::Missed,
                "o computador estava suspenso ou ocupado",
            ),
        ] {
            if !times.is_empty() {
                out.push(Action::Late {
                    index,
                    status,
                    times,
                    why,
                });
            }
        }
        if on_time {
            out.push(Action::Run { index, at: latest });
        }
    }
    out
}

fn fmt(ts: i64) -> String {
    local(ts).format("%d/%m %H:%M").to_string()
}

async fn record(state: &AppState, d: &Due, at: i64, status: RunStatus, note: String) {
    let r = RunRecord {
        id: Uuid::now_v7(),
        snippet_id: d.snippet_id,
        label: d.label.clone(),
        origin: RunOrigin::Scheduled,
        status,
        target: String::new(),
        started_at: at,
        finished_at: None,
        results: Vec::new(),
        note: Some(note),
    };
    let _ = state.app.emit("snippets://run", &r);
    runs::save(state, r).await;
}

fn notify_failure(state: &AppState, label: &str, r: &RunRecord) {
    let failed: Vec<&str> = r
        .results
        .iter()
        .filter(|t| t.exit_code != Some(0) || t.error.is_some())
        .map(|t| t.target.as_str())
        .collect();
    notify(
        state,
        &format!("\"{label}\" falhou"),
        &format!("Agendamento falhou em {}.", failed.join(", ")),
    );
}

pub fn notify(state: &AppState, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = state
        .app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        tracing::warn!("notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> i64 {
        let n = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap();
        Local
            .from_local_datetime(&n)
            .earliest()
            .unwrap()
            .timestamp()
    }

    fn every(minutes: u32) -> Vec<Due> {
        vec![Due {
            snippet_id: Uuid::nil(),
            label: "s".into(),
            cron: Cron::parse(&format!("*/{minutes} * * * *")).unwrap(),
            since: 0,
        }]
    }

    #[test]
    fn a_new_schedule_does_not_report_the_past() {
        // Last tick at 08:00 (before Vessel was closed); this schedule was first seen at 10:00:10.
        let mut due = every(30);
        due[0].since = ts("2026-06-10 10:00:10");
        let a = plan(&due, ts("2026-06-10 08:00:00"), ts("2026-06-10 10:00:20"), false, ts("2026-06-10 10:00:05"), &[]);
        assert!(a.is_empty(), "{a:?}");
        let a = plan(&due, ts("2026-06-10 10:00:20"), ts("2026-06-10 10:30:05"), false, ts("2026-06-10 10:00:05"), &[]);
        assert_eq!(a, vec![Action::Run { index: 0, at: ts("2026-06-10 10:30:00") }]);
    }

    #[test]
    fn a_time_that_just_came_runs() {
        let a = plan(
            &every(5),
            ts("2026-06-10 10:04:50"),
            ts("2026-06-10 10:05:05"),
            false,
            0,
            &[],
        );
        assert_eq!(
            a,
            vec![Action::Run {
                index: 0,
                at: ts("2026-06-10 10:05:00")
            }]
        );
        assert!(
            plan(
                &every(5),
                ts("2026-06-10 10:05:05"),
                ts("2026-06-10 10:05:20"),
                false,
                0,
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn paused_schedules_do_nothing() {
        assert!(
            plan(
                &every(5),
                ts("2026-06-10 10:04:50"),
                ts("2026-06-10 10:05:05"),
                true,
                0,
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn a_time_while_locked_does_not_run_right_after_unlocking() {
        // 10:30 came while locked; the vault opened at 10:30:40 (first unlocked tick 10:30:50).
        let locked = [(ts("2026-06-10 10:01:00"), ts("2026-06-10 10:30:50"))];
        let a = plan(&every(5), ts("2026-06-10 10:00:50"), ts("2026-06-10 10:30:50"), false, ts("2026-06-10 09:00:00"), &locked);
        assert!(!a.iter().any(|x| matches!(x, Action::Run { .. })), "{a:?}");
        // The next time runs normally.
        let a = plan(&every(5), ts("2026-06-10 10:30:50"), ts("2026-06-10 10:35:05"), false, ts("2026-06-10 09:00:00"), &locked);
        assert_eq!(a, vec![Action::Run { index: 0, at: ts("2026-06-10 10:35:00") }]);
    }

    #[test]
    fn times_while_locked_are_skipped_not_run_late() {
        // Locked from 10:01 to 10:33, unlocked at 10:33:10: 10:05..10:30 were skipped, none runs late.
        let locked = [(ts("2026-06-10 10:01:00"), ts("2026-06-10 10:33:00"))];
        let a = plan(
            &every(5),
            ts("2026-06-10 10:00:50"),
            ts("2026-06-10 10:33:10"),
            false,
            ts("2026-06-10 09:00:00"),
            &locked,
        );
        assert_eq!(a.len(), 1, "{a:?}");
        let Action::Late {
            status, times, why, ..
        } = &a[0]
        else {
            panic!("{a:?}")
        };
        assert_eq!(*status, RunStatus::Skipped);
        assert_eq!(times.len(), 6);
        assert!(why.contains("bloqueado"));
    }

    #[test]
    fn times_while_closed_are_missed_and_the_current_one_runs() {
        // Vessel closed 08:00-09:59:50 (started then); at 10:00:10 the 10:00 time runs.
        let a = plan(
            &every(30),
            ts("2026-06-10 08:00:00"),
            ts("2026-06-10 10:00:10"),
            false,
            ts("2026-06-10 09:59:50"),
            &[],
        );
        assert_eq!(a.len(), 2, "{a:?}");
        let Action::Late { status, times, .. } = &a[0] else {
            panic!("{a:?}")
        };
        assert_eq!((*status, times.len()), (RunStatus::Missed, 3)); // 08:30, 09:00, 09:30
        assert_eq!(
            a[1],
            Action::Run {
                index: 0,
                at: ts("2026-06-10 10:00:00")
            }
        );
    }

    #[test]
    fn a_time_just_before_opening_vessel_is_missed() {
        let a = plan(&every(30), ts("2026-06-10 08:00:00"), ts("2026-06-10 10:00:10"), false, ts("2026-06-10 10:00:05"), &[]);
        assert!(!a.iter().any(|x| matches!(x, Action::Run { .. })), "{a:?}");
    }

    #[test]
    fn a_long_sleep_is_reported_once_per_reason() {
        let a = plan(
            &every(1),
            ts("2026-06-10 10:00:00"),
            ts("2026-06-11 10:00:30"),
            false,
            0,
            &[],
        );
        let late: Vec<_> = a
            .iter()
            .filter(|x| matches!(x, Action::Late { .. }))
            .collect();
        assert_eq!(late.len(), 1);
        let Action::Late { status, times, .. } = late[0] else {
            unreachable!()
        };
        assert_eq!((*status, times.len()), (RunStatus::Missed, 1439));
    }
}
