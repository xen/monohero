//! Read-only chat observation. Timers belong here, never in the FIFO scheduler.
use crate::{
    call, db,
    goal::Rpc,
    management::{self, Control},
    Job, Request, Shared,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Config {
    codex: PathBuf,
    proxy: bool,
    socket: Option<PathBuf>,
    interval: u64,
    enabled: bool,
}
pub fn configuration(home: &Path) -> Result<Value> {
    match fs::read(home.join("monitor.json")) {
        Ok(body) => Ok(serde_json::from_slice(&body)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({"enabled":false})),
        Err(e) => Err(e.into()),
    }
}
pub fn configure(
    s: &Shared,
    codex: &Path,
    proxy: bool,
    socket: Option<PathBuf>,
    interval: u64,
    enabled: bool,
) -> Result<()> {
    if !(5..=3600).contains(&interval) {
        bail!("monitor interval must be 5–3600 seconds");
    }
    if socket.is_some() && !proxy {
        bail!("a control socket requires explicit proxy mode");
    }
    let codex = if enabled {
        crate::chat::executable(codex)?
    } else {
        codex.to_path_buf()
    };
    let temporary = s.home.join("monitor.json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(
        &mut file,
        &Config {
            codex,
            proxy,
            socket,
            interval,
            enabled,
        },
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, s.home.join("monitor.json"))?;
    start_configured(&s.home, s.monitor_started.clone())
}
pub fn start_configured(home: &Path, started: Arc<AtomicBool>) -> Result<()> {
    if configuration(home)?["enabled"] != true {
        return Ok(());
    }
    if started
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }
    let home = home.to_owned();
    thread::spawn(move || {
        let result = (|| -> Result<()> {
            loop {
                let config: Config = serde_json::from_value(configuration(&home)?)?;
                if !config.enabled {
                    return Ok(());
                }
                if let Err(error) =
                    round(&home, &config.codex, config.proxy, config.socket.as_deref())
                {
                    eprintln!("chat monitor: {error:#}");
                }
                thread::sleep(Duration::from_secs(config.interval));
            }
        })();
        if let Err(error) = result {
            eprintln!("chat monitor stopped explicitly: {error:#}");
        }
        started.store(false, Ordering::SeqCst);
    });
    Ok(())
}
pub fn run(
    home: &Path,
    codex: &Path,
    proxy: bool,
    socket: Option<&Path>,
    interval: u64,
    once: bool,
) -> Result<()> {
    if !(5..=3600).contains(&interval) {
        bail!("monitor interval must be 5–3600 seconds");
    }
    loop {
        round(home, codex, proxy, socket)?;
        if once {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(interval));
    }
}
fn round(home: &Path, codex: &Path, proxy: bool, socket: Option<&Path>) -> Result<()> {
    let status = call(home, &Request::Status { queue: None })?;
    let jobs: Vec<Job> = serde_json::from_value(status)?;
    let mut client = Rpc::connect(codex, proxy, socket);
    let mut cache = HashMap::<String, Value>::new();
    for job in jobs.iter().filter(|job| {
        !job.archived && matches!(job.state.as_str(), "running" | "queued" | "deferred")
    }) {
        let Some(thread) = management::thread_id(job) else {
            continue;
        };
        let observation = cache
            .entry(thread.clone())
            .or_insert_with(|| match client.as_mut() {
                Ok(client) => match probe(client, &thread, proxy) {
                    Ok(value) => value,
                    Err(error) => unknown(&thread, &format!("{error:#}")),
                },
                Err(error) => unknown(&thread, &format!("{error:#}")),
            })
            .clone();
        call(
            home,
            &Request::Manage {
                action: Control::Observe {
                    id: job.id,
                    observation,
                },
            },
        )?;
    }
    Ok(())
}
fn unknown(thread: &str, reason: &str) -> Value {
    json!({"thread_id":thread,"state":"unknown","source":"unavailable","observed_at":now(),"reason":reason.chars().take(1000).collect::<String>(),"authoritative":false})
}
fn probe(client: &mut Rpc, thread: &str, proxy: bool) -> Result<Value> {
    let raw = client.request(
        "thread/read",
        json!({"threadId":thread,"includeTurns":false}),
    )?;
    let data = raw.get("thread").context("missing thread snapshot")?;
    if data["id"] != thread {
        bail!("RPC returned a different chat");
    }
    let page = client.request(
        "thread/turns/list",
        json!({"threadId":thread,"limit":1,"itemsView":"summary","sortDirection":"desc"}),
    )?;
    let latest = page["data"]
        .as_array()
        .and_then(|turns| turns.first())
        .cloned()
        .unwrap_or(Value::Null);
    let goal = client.request("thread/goal/get", json!({"threadId":thread}))?;
    Ok(classify(thread, data, &latest, &goal["goal"], proxy, now()))
}
fn classify(
    thread: &str,
    data: &Value,
    latest: &Value,
    goal: &Value,
    proxy: bool,
    observed: u64,
) -> Value {
    let runtime = data["status"]["type"].as_str().unwrap_or("unknown");
    let authoritative = proxy && matches!(runtime, "active" | "idle" | "systemError");
    let flags = data["status"]["activeFlags"].as_array();
    let has = |flag: &str| flags.is_some_and(|flags| flags.iter().any(|item| item == flag));
    let (state, reason) = if authoritative && has("waitingOnApproval") {
        ("needs_approval", "Chat is waiting for approval")
    } else if authoritative && has("waitingOnUserInput") {
        ("needs_input", "Chat is waiting for user input")
    } else if authoritative && runtime == "systemError" {
        ("stopped", "Owning host reports a runtime error")
    } else if goal["status"] == "blocked" {
        ("blocked", "Saved goal is blocked")
    } else if matches!(latest["status"].as_str(), Some("failed" | "interrupted")) {
        ("stopped", "Latest persisted turn failed or was interrupted")
    } else if authoritative && runtime == "active" || latest["status"] == "inProgress" {
        ("working", "Turn is in progress")
    } else if authoritative && runtime == "idle" || latest["status"] == "completed" {
        (
            "idle",
            "Latest turn completed; inspect background work before releasing legacy claims",
        )
    } else {
        (
            "unknown",
            "Owning runtime is not attached; inactivity is not proved",
        )
    };
    json!({"thread_id":thread,"title":data["name"],"state":state,"reason":reason,"source":if authoritative{"codex-runtime"}else{"codex-history"},"authoritative":authoritative,"observed_at":observed,"runtime_state":runtime,"turn_id":latest["id"],"turn_started_at":latest["startedAt"],"activity_at":latest["completedAt"].as_u64().or_else(||data["updatedAt"].as_u64()),"goal_status":goal["status"]})
}
pub fn validate(observation: &Value) -> Result<()> {
    let stamp = observation["observed_at"]
        .as_u64()
        .context("observation needs a timestamp")?;
    if stamp.abs_diff(now()) > 60 {
        bail!("stale or future-dated observation");
    }
    if ![
        "working",
        "idle",
        "stopped",
        "needs_input",
        "needs_approval",
        "blocked",
        "unknown",
    ]
    .contains(
        &observation["state"]
            .as_str()
            .context("missing chat state")?,
    ) {
        bail!("unknown chat state");
    }
    if observation["authoritative"].as_bool().is_none() {
        bail!("observation must declare authority");
    }
    Ok(())
}
pub fn evaluate(s: &Shared, job: &Job, observation: &Value) -> Result<()> {
    if job.pending_action.is_some() || job.archived {
        return Ok(());
    }
    let state = observation["state"].as_str().unwrap_or("unknown");
    let blocked = matches!(
        state,
        "stopped" | "needs_input" | "needs_approval" | "blocked"
    );
    let authority = observation["authoritative"] == true;
    let c = db(&s.home)?;
    if job.state == "queued" && job.hold_reason.is_some() {
        let old = job.observation.as_ref();
        let changed = old.is_some_and(|old| old["state"] != observation["state"]);
        let new_activity = observation["activity_at"]
            .as_u64()
            .zip(old.and_then(|old| old["observed_at"].as_u64()))
            .is_some_and(|(activity, blocked_at)| activity > blocked_at);
        if !blocked
            && matches!(state, "idle" | "working")
            && ((authority && changed) || new_activity)
        {
            c.execute(
                "UPDATE jobs SET hold_reason=NULL WHERE id=?1 AND state='queued'",
                [job.id],
            )?;
        }
        return Ok(());
    }
    if job.state == "queued" && blocked && (authority || observation["goal_status"] == "blocked") {
        management::request_release(
            s,
            job.id,
            "defer",
            observation["reason"]
                .as_str()
                .unwrap_or("Chat needs attention"),
            None,
        )?;
    } else if job.state == "running" {
        let Some(r) = management::receipt(&s.home, job.id)? else {
            return Ok(());
        };
        let grace: u64 = c.query_row(
            "SELECT unclaimed_after FROM queues WHERE name=?1",
            [&job.queue],
            |row| row.get(0),
        )?;
        let no_claim = r["claimed_at"].is_null()
            && r["updated_at"]
                .as_u64()
                .is_some_and(|time| now().saturating_sub(time) >= grace);
        let current_turn = observation["turn_started_at"]
            .as_u64()
            .is_some_and(|start| {
                start.saturating_add(30) >= r["claimed_at"].as_u64().unwrap_or(u64::MAX)
            });
        let managed = r["managed"] == true;
        if no_claim || blocked && managed && (authority || current_turn) {
            let reason = if no_claim {
                "Chat did not claim its invitation within the configured grace period"
            } else {
                observation["reason"]
                    .as_str()
                    .unwrap_or("Chat needs attention")
            };
            management::request_release(s, job.id, "defer", reason, None)?;
        }
    }
    Ok(())
}
pub fn remove_pending(codex: &Path, thread: &str, message: &str) -> Result<()> {
    let mut client = Rpc::start(codex)?;
    let mut cursor = Value::Null;
    loop {
        let page = client.request(
            "thread/queue/list",
            json!({"threadId":thread,"limit":100,"cursor":cursor}),
        )?;
        let found = page["data"]
            .as_array()
            .context("invalid queued-message list")?
            .iter()
            .any(|entry| entry["id"] == message);
        if found {
            client.request(
                "thread/queue/delete",
                json!({"threadId":thread,"queuedSubmissionId":message}),
            )?;
            return Ok(());
        }
        let next = page["nextCursor"].clone();
        if next.is_null() {
            return Ok(());
        }
        if next == cursor {
            bail!("queued-message pagination did not advance; withdrawal uncertain");
        }
        cursor = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_reader_never_claims_owning_runtime_authority() {
        let data = json!({"status":{"type":"notLoaded"},"updatedAt":10});
        let value = classify("chat", &data, &Value::Null, &Value::Null, false, 20);
        assert_eq!(value["state"], "unknown");
        assert_eq!(value["authoritative"], false);
        assert_eq!(value["source"], "codex-history");
        let value = classify(
            "chat",
            &data,
            &json!({"status":"completed"}),
            &Value::Null,
            false,
            20,
        );
        assert_eq!(value["state"], "idle");
        assert_eq!(value["authoritative"], false);
    }
    #[test]
    fn approval_flags_require_explicit_owning_proxy() {
        let data = json!({"status":{"type":"active","activeFlags":["waitingOnApproval"]}});
        assert_eq!(
            classify("c", &data, &Value::Null, &Value::Null, false, 20)["state"],
            "unknown"
        );
        assert_eq!(
            classify("c", &data, &Value::Null, &Value::Null, true, 20)["state"],
            "needs_approval"
        );
        let data = json!({"status":{"type":"active","activeFlags":["waitingOnUserInput"]}});
        assert_eq!(
            classify("c", &data, &Value::Null, &Value::Null, true, 20)["state"],
            "needs_input"
        );
    }
    #[test]
    fn failed_history_and_blocked_goal_are_labelled_history() {
        let data = json!({"status":{"type":"notLoaded"}});
        for (turn, goal, state) in [
            (json!({"status":"failed"}), Value::Null, "stopped"),
            (Value::Null, json!({"status":"blocked"}), "blocked"),
        ] {
            let value = classify("c", &data, &turn, &goal, false, 20);
            assert_eq!(value["state"], state);
            assert_eq!(value["authoritative"], false);
        }
    }
    #[test]
    fn stale_observations_and_missing_authority_are_rejected() {
        let mut value = unknown("c", "unavailable");
        assert!(validate(&value).is_ok());
        value["observed_at"] = json!(now() - 61);
        assert!(validate(&value).is_err());
        value["observed_at"] = json!(now());
        value.as_object_mut().unwrap().remove("authoritative");
        assert!(validate(&value).is_err());
    }
}
