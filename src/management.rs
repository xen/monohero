//! Durable queue controls shared by the native UI, monitor and CLI.
use crate::{call, db, jobs, name, signal, Job, Request, Shared};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Subcommand)]
pub enum Actions {
    /// Explicitly upgrade future native workers submitted with this older executable.
    AdoptWorkers {
        #[arg(long)]
        from: PathBuf,
    },
    /// Configure how a queue handles chats that need attention.
    Policy {
        #[arg(long)]
        queue: String,
        #[arg(long,value_parser=["attention","tail"])]
        blocked: String,
        #[arg(long, default_value_t = 180)]
        unclaimed_after: u64,
    },
    /// Reorder all queued jobs in one queue using a comma-separated list of IDs.
    Reorder {
        #[arg(long)]
        queue: String,
        #[arg(long, value_delimiter = ',')]
        ids: Vec<i64>,
    },
    /// Defer a job. An owner yielding legacy claimed work must pass its thread ID.
    Defer {
        id: i64,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        thread: Option<String>,
    },
    /// Resume deferred work as a fresh grant at the tail.
    Resume { id: i64 },
    /// Pause dispatch in a queue; running work is unaffected.
    PauseQueue { queue: String },
    /// Resume dispatch in a paused queue.
    ResumeQueue { queue: String },
    /// Clear pending jobs/history; refuses active work.
    Clear {
        #[arg(long)]
        queue: String,
        #[arg(long)]
        history_only: bool,
        #[arg(long)]
        delete: bool,
        #[arg(long)]
        acknowledge_failures: bool,
    },
    /// Probe chat status in a native process without model calls.
    Monitor {
        #[arg(long, default_value = "codex")]
        codex: PathBuf,
        #[arg(long)]
        proxy: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long, default_value_t = 15)]
        interval: u64,
        #[arg(long)]
        once: bool,
    },
    /// Run a protected command under the current chat grant's fencing lock.
    Run {
        id: i64,
        #[arg(long)]
        thread: String,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Configure native chat polling for the queue service.
    ConfigureMonitor {
        #[arg(long, default_value = "codex")]
        codex: PathBuf,
        #[arg(long)]
        proxy: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long, default_value_t = 15)]
        interval: u64,
        #[arg(long)]
        disabled: bool,
    },
    /// Read a redacted management snapshot.
    Inspect,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    AdoptWorkers {
        from: PathBuf,
    },
    Policy {
        queue: String,
        blocked: String,
        unclaimed_after: u64,
    },
    Reorder {
        queue: String,
        ids: Vec<i64>,
    },
    Defer {
        id: i64,
        reason: String,
        #[serde(default)]
        owner: Option<String>,
    },
    Cancel {
        id: i64,
    },
    CancelArchived {
        id: i64,
    },
    Resume {
        id: i64,
    },
    Pause {
        queue: String,
        paused: bool,
    },
    Clear {
        queue: String,
        #[serde(default)]
        history_only: bool,
        #[serde(default)]
        delete: bool,
        #[serde(default)]
        acknowledge_failures: bool,
    },
    ConfigureMonitor {
        codex: PathBuf,
        proxy: bool,
        socket: Option<PathBuf>,
        interval: u64,
        enabled: bool,
    },
    Resolve {
        id: i64,
        #[serde(default)]
        acknowledge_failures: bool,
    },
    ClearAll {
        #[serde(default)]
        history_only: bool,
        #[serde(default)]
        delete: bool,
        #[serde(default)]
        acknowledge_failures: bool,
    },
    Observe {
        id: i64,
        observation: Value,
    },
}
pub fn run(action: Actions, home: &Path) -> Result<()> {
    let control = match action {
        Actions::AdoptWorkers { from } => Control::AdoptWorkers { from },
        Actions::Policy {
            queue,
            blocked,
            unclaimed_after,
        } => Control::Policy {
            queue,
            blocked,
            unclaimed_after,
        },
        Actions::Reorder { queue, ids } => Control::Reorder { queue, ids },
        Actions::Defer { id, reason, thread } => Control::Defer {
            id,
            reason,
            owner: thread,
        },
        Actions::Resume { id } => Control::Resume { id },
        Actions::PauseQueue { queue } => Control::Pause {
            queue,
            paused: true,
        },
        Actions::ResumeQueue { queue } => Control::Pause {
            queue,
            paused: false,
        },
        Actions::Clear {
            queue,
            history_only,
            delete,
            acknowledge_failures,
        } => Control::Clear {
            queue,
            history_only,
            delete,
            acknowledge_failures,
        },
        Actions::Monitor {
            codex,
            proxy,
            socket,
            interval,
            once,
        } => return crate::monitor::run(home, &codex, proxy, socket.as_deref(), interval, once),
        Actions::Run {
            id,
            thread,
            command,
        } => return crate::fence::run(home, id, &thread, &command),
        Actions::ConfigureMonitor {
            codex,
            proxy,
            socket,
            interval,
            disabled,
        } => Control::ConfigureMonitor {
            codex,
            proxy,
            socket,
            interval,
            enabled: !disabled,
        },
        Actions::Inspect => {
            println!(
                "{}",
                serde_json::to_string_pretty(&call(home, &Request::Snapshot)?)?
            );
            return Ok(());
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&call(home, &Request::Manage { action: control })?)?
    );
    Ok(())
}
pub fn migrate(c: &Connection) -> Result<()> {
    let columns = c
        .prepare("PRAGMA table_info(jobs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    for (column, definition) in [
        ("position", "INTEGER NOT NULL DEFAULT 0"),
        ("archived", "INTEGER NOT NULL DEFAULT 0"),
        ("parent_id", "INTEGER"),
        ("hold_reason", "TEXT"),
        ("pending_action", "TEXT"),
        ("observation", "TEXT"),
    ] {
        if !columns.contains(column) {
            c.execute_batch(&format!(
                "ALTER TABLE jobs ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    c.execute_batch("UPDATE jobs SET position=id WHERE position=0; CREATE TABLE IF NOT EXISTS queues(name TEXT PRIMARY KEY,paused INTEGER NOT NULL DEFAULT 0,archived INTEGER NOT NULL DEFAULT 0,blocked_policy TEXT NOT NULL DEFAULT 'attention',unclaimed_after INTEGER NOT NULL DEFAULT 180); INSERT OR IGNORE INTO queues(name) SELECT DISTINCT queue FROM jobs;")?;
    Ok(())
}
pub fn thread_id(job: &Job) -> Option<String> {
    let start = 3;
    if !matches!(
        job.command.get(start).map(String::as_str),
        Some("chat-worker" | "notify-worker")
    ) {
        return None;
    }
    job.command[start..]
        .windows(2)
        .find(|args| args[0] == "--thread")
        .map(|args| args[1].clone())
}
pub fn receipt(home: &Path, id: i64) -> Result<Option<Value>> {
    match fs::read(home.join("chats").join(format!("{id}.json"))) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub fn snapshot(s: &Shared) -> Result<Value> {
    let c = db(&s.home)?;
    let mut queue_rows=c.prepare("SELECT name,paused,blocked_policy,unclaimed_after FROM queues WHERE archived=0 ORDER BY name")?;
    let queues=queue_rows.query_map([],|r|Ok(json!({"name":r.get::<_,String>(0)?,"paused":r.get::<_,bool>(1)?,"blocked_policy":r.get::<_,String>(2)?,"unclaimed_after":r.get::<_,i64>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut visible = Vec::new();
    let records = jobs(&c)?;
    for job in records.iter().filter(|job| !job.archived) {
        let mut origin = job;
        while let Some(parent) = origin.parent_id {
            if parent >= origin.id {
                bail!("invalid resume ancestry for job {}", job.id);
            }
            origin = records
                .iter()
                .find(|item| item.id == parent)
                .context("missing original job")?;
        }
        let mut item = serde_json::to_value(job)?;
        let object = item.as_object_mut().unwrap();
        object.insert("label".into(), json!(origin.key));
        object.remove("command");
        object.remove("callback");
        object.remove("log");
        object.insert(
            "protected_failure".into(),
            json!(crate::fence::has_failure(&s.home, job.id)),
        );
        object.insert("thread_id".into(), json!(thread_id(job)));
        if let Some(r) = receipt(&s.home, job.id)? {
            object.insert("delivery".into(),json!({"state":r["state"],"claimed_at":r["claimed_at"],"managed":r["managed"],"error":r["error"],"app_wake":r["app_wake"]}));
        }
        visible.push(item);
    }
    Ok(
        json!({"version":env!("CARGO_PKG_VERSION"),"queues":queues,"jobs":visible,"monitor":crate::monitor::configuration(&s.home)?}),
    )
}
fn find(c: &Connection, id: i64) -> Result<Job> {
    jobs(c)?
        .into_iter()
        .find(|job| job.id == id)
        .context("unknown job")
}
fn transaction(c: &Connection, operation: impl FnOnce() -> Result<Value>) -> Result<Value> {
    c.execute_batch("BEGIN IMMEDIATE")?;
    match operation() {
        Ok(value) => {
            c.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            c.execute_batch("ROLLBACK")?;
            Err(error)
        }
    }
}
pub fn apply(action: Control, s: &Shared) -> Result<Value> {
    let c = db(&s.home)?;
    let result = match action {
        Control::AdoptWorkers { from } => {
            if !from.is_absolute() {
                bail!("the old worker executable must be absolute");
            }
            let from = fs::canonicalize(from)?;
            if !from.is_file() {
                bail!("the old worker executable is not a file");
            }
            let path = s.home.join("worker-adoptions.json");
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let guard = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(s.home.join("worker-adoptions.lock"))?;
            fs2::FileExt::lock_exclusive(&guard)?;
            let mut config: std::collections::BTreeMap<PathBuf, PathBuf> = match fs::read(&path) {
                Ok(body) => serde_json::from_slice(&body)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
                Err(e) => return Err(e.into()),
            };
            let to = fs::canonicalize(std::env::current_exe()?)?;
            config.insert(from.clone(), to.clone());
            let temporary = s.home.join("worker-adoptions.json.tmp");
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temporary)?;
            serde_json::to_writer_pretty(&mut file, &config)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(temporary, path)?;
            json!({"from":from,"to":to,"scope":"future native workers only; running workers and submitted arguments remain unchanged"})
        }
        Control::Policy {
            queue,
            blocked,
            unclaimed_after,
        } => {
            name(&queue)?;
            if !["attention", "tail"].contains(&blocked.as_str())
                || !(30..=86400).contains(&unclaimed_after)
            {
                bail!("invalid blocked policy or grace period (30–86400 seconds)");
            }
            c.execute("INSERT INTO queues(name,blocked_policy,unclaimed_after) VALUES(?1,?2,?3) ON CONFLICT(name) DO UPDATE SET blocked_policy=?2,unclaimed_after=?3,archived=0",params![queue,blocked,unclaimed_after])?;
            json!({"queue":queue,"blocked_policy":blocked,"unclaimed_after":unclaimed_after})
        }
        Control::Pause { queue, paused } => {
            if c.execute(
                "UPDATE queues SET paused=?1 WHERE name=?2 AND archived=0",
                params![paused, queue],
            )? != 1
            {
                bail!("unknown queue");
            }
            json!({"queue":queue,"paused":paused})
        }
        Control::Reorder { queue, ids } => transaction(&c, || {
            if c.query_row(
                "SELECT COUNT(*) FROM queues WHERE name=?1 AND archived=0",
                [&queue],
                |row| row.get::<_, i64>(0),
            )? != 1
            {
                bail!("unknown queue");
            }
            let actual: HashSet<i64> = jobs(&c)?
                .iter()
                .filter(|j| j.queue == queue && j.state == "queued" && !j.archived)
                .map(|j| j.id)
                .collect();
            if actual.len() != ids.len() || actual != ids.iter().copied().collect() {
                bail!("queue changed or IDs include non-pending/foreign jobs; refresh before reordering");
            }
            for (position, id) in ids.iter().enumerate() {
                c.execute(
                    "UPDATE jobs SET position=?1 WHERE id=?2",
                    params![position as i64 + 1, id],
                )?;
            }
            Ok(json!({"queue":queue,"order":ids}))
        })?,
        Control::Resume { id } => transaction(&c, || {
            let job = find(&c, id)?;
            if job.state != "deferred" && !(job.state == "queued" && job.hold_reason.is_some()) {
                bail!("only deferred or held jobs can be resumed");
            }
            let next = copy_job(&c, &s.home, &job, None)?;
            c.execute(
                "UPDATE jobs SET state='requeued',archived=1 WHERE id=?1",
                [id],
            )?;
            Ok(json!({"id":id,"new_job":next,"state":"queued"}))
        })?,
        Control::Defer { id, reason, owner } => {
            return request_release(s, id, "defer", &reason, owner.as_deref())
        }
        Control::Cancel { id } => {
            return request_release(s, id, "cancel", "Cancelled by operator", None)
        }
        Control::CancelArchived { id } => {
            return request_release(s, id, "cancel", "Chat archived", None)
        }
        Control::Clear {
            queue,
            history_only,
            delete,
            acknowledge_failures,
        } => transaction(&c, || {
            clear_records(&c, &[queue], history_only, delete, acknowledge_failures)
        })?,
        Control::ClearAll {
            history_only,
            delete,
            acknowledge_failures,
        } => transaction(&c, || {
            let queues = c
                .prepare("SELECT name FROM queues WHERE archived=0")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            clear_records(&c, &queues, history_only, delete, acknowledge_failures)
        })?,
        Control::Resolve {
            id,
            acknowledge_failures,
        } => {
            if !acknowledge_failures {
                bail!("inspect effects and acknowledge the failure before resolving it");
            }
            if c.execute("UPDATE jobs SET state='resolved' WHERE id=?1 AND state IN ('failed','interrupted')",[id])?!=1 {bail!("only failed/interrupted jobs can be resolved");}
            json!({"id":id,"state":"resolved"})
        }
        Control::ConfigureMonitor {
            codex,
            proxy,
            socket,
            interval,
            enabled,
        } => {
            crate::monitor::configure(s, &codex, proxy, socket, interval, enabled)?;
            json!({"monitor":crate::monitor::configuration(&s.home)?})
        }
        Control::Observe { id, observation } => {
            let job = find(&c, id)?;
            if thread_id(&job).as_deref() != observation["thread_id"].as_str() {
                bail!("observation does not match destination chat");
            }
            crate::monitor::validate(&observation)?;
            c.execute(
                "UPDATE jobs SET observation=?1 WHERE id=?2",
                params![serde_json::to_string(&observation)?, id],
            )?;
            crate::monitor::evaluate(s, &job, &observation)?;
            json!({"id":id,"observed":true})
        }
    };
    signal(s);
    Ok(result)
}
pub fn request_release(
    s: &Shared,
    id: i64,
    kind: &str,
    reason: &str,
    owner: Option<&str>,
) -> Result<Value> {
    if reason.trim().is_empty() || reason.len() > 2000 {
        bail!("a concise nonempty reason is required");
    }
    let c = db(&s.home)?;
    let job = find(&c, id)?;
    if matches!(job.state.as_str(), "queued" | "deferred") {
        let result = transaction(&c, || {
            let current = find(&c, id)?;
            if !matches!(current.state.as_str(), "queued" | "deferred") {
                bail!("job started; refresh before release");
            }
            if kind == "cancel" {
                c.execute(
                    "UPDATE jobs SET state='cancelled',hold_reason=?1 WHERE id=?2",
                    params![reason, id],
                )?;
            } else {
                defer_record(&c, &s.home, &current, reason)?;
            }
            Ok(json!({"id":id,"state":find(&c,id)?.state}))
        })?;
        signal(s);
        return Ok(result);
    }
    if job.state != "running" {
        bail!("only pending or running jobs can be deferred/cancelled");
    }
    let r = receipt(&s.home, id)?
        .context("running command jobs cannot be remotely revoked; wait for command completion")?;
    if r["kind"] != "lease" {
        bail!("completion notifications cannot be deferred as release jobs");
    }
    if let Some(owner) = owner {
        if r["thread_id"] != owner {
            bail!("thread does not own this grant");
        }
    }
    let managed = r["managed"].as_bool() == Some(true);
    if !r["claimed_at"].is_null() && !managed && owner.is_none() {
        bail!("legacy claimed work requires its owner to stop children and yield with --thread; inactivity is not safe revocation evidence");
    }
    let changed=c.execute("UPDATE jobs SET pending_action=?1,hold_reason=?2 WHERE id=?3 AND state='running' AND pending_action IS NULL",params![kind,reason,id])?;
    if changed == 0 {
        return Ok(json!({"id":id,"state":"draining"}));
    }
    if managed {
        crate::fence::stop(&s.home, id)?;
    }
    let home = s.home.clone();
    let thread = r["thread_id"]
        .as_str()
        .context("missing destination thread")?
        .to_owned();
    let operation = if kind == "defer" { "defer" } else { "cancel" };
    let owner_yield = owner.is_some();
    thread::spawn(move || {
        let result = (|| -> Result<()> {
            let _resource = if managed {
                Some(crate::fence::exclusive(&home, id)?)
            } else {
                None
            };
            if !owner_yield && crate::fence::has_failure(&home, id) {
                bail!("a protected command failed; its owner must inspect effects and yield with --thread");
            }
            crate::chat::revoke(&home, id, &thread, operation, owner_yield)
        })();
        if let Err(error) = result {
            if let Ok(c) = db(&home) {
                let _=c.execute("UPDATE jobs SET pending_action=NULL,hold_reason=?1 WHERE id=?2 AND state='running'",params![format!("Release failed; inspect: {error:#}"),id]);
            }
        }
    });
    signal(s);
    Ok(json!({"id":id,"state":"draining","reason":reason}))
}
fn copy_job(c: &Connection, home: &Path, job: &Job, hold: Option<&str>) -> Result<i64> {
    let serial: SystemTime = SystemTime::now();
    let key = format!(
        "resume-{}-{}",
        job.id,
        serial.duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let mut command = job.command.clone();
    if command.get(3).map(String::as_str) == Some("chat-worker") {
        let worker = 3;
        if command.iter().any(|arg| arg == "--park-goal") {
            let thread = thread_id(job).context("goal wait lacks destination thread")?;
            let saved: Value = serde_json::from_slice(&fs::read(
                home.join("goals").join(format!("{thread}.json")),
            )?)?;
            match saved["state"].as_str() {
                Some("restored") => command.retain(|arg| arg != "--park-goal"),
                Some("parked") => {
                    if !command.iter().any(|arg| arg == "--goal-key") {
                        command.extend(["--goal-key".into(), job.key.clone()]);
                    }
                }
                _ => bail!("goal wait is uncertain; inspect its saved lifecycle before resuming"),
            }
        }
        if let Some(offset) = command[worker..].iter().position(|arg| arg == "--key") {
            command[worker + offset + 1] = key.clone();
        }
    }
    c.execute("INSERT INTO jobs(queue,key,cwd,command,state,callback,callback_state,position,parent_id,hold_reason,observation) VALUES(?1,?2,?3,?4,'queued',?5,'none',(SELECT COALESCE(MAX(position),0)+1 FROM jobs WHERE queue=?1),?6,?7,?8)",params![job.queue,key,job.cwd.to_str(),serde_json::to_string(&command)?,job.callback.as_ref().and_then(|p|p.to_str()),job.id,hold,job.observation.as_ref().map(serde_json::to_string).transpose()?])?;
    Ok(c.last_insert_rowid())
}
fn defer_record(c: &Connection, home: &Path, job: &Job, reason: &str) -> Result<()> {
    let policy: String = c.query_row(
        "SELECT blocked_policy FROM queues WHERE name=?1",
        [&job.queue],
        |r| r.get(0),
    )?;
    c.execute(
        "UPDATE jobs SET state='deferred',pending_action=NULL,hold_reason=?1 WHERE id=?2",
        params![reason, job.id],
    )?;
    if policy == "tail" {
        if job.state == "queued" {
            c.execute("UPDATE jobs SET state='queued',position=(SELECT COALESCE(MAX(position),0)+1 FROM jobs WHERE queue=?1) WHERE id=?2",params![job.queue,job.id])?;
        } else {
            copy_job(c, home, job, Some(reason))?;
            c.execute(
                "UPDATE jobs SET state='requeued',archived=1 WHERE id=?1",
                [job.id],
            )?;
        }
    }
    Ok(())
}
pub fn complete_release(c: &Connection, home: &Path, id: i64, code: Option<i32>) -> Result<bool> {
    let job = find(c, id)?;
    let Some(action) = job.pending_action.as_deref() else {
        return Ok(false);
    };
    // A failed revocation must never silently turn a real job failure into a deferral.
    if code != Some(0) {
        return Ok(false);
    }
    let expected = if action == "defer" {
        "deferred"
    } else {
        "cancelled"
    };
    if receipt(home, id)?
        .as_ref()
        .and_then(|r| r["state"].as_str())
        != Some(expected)
    {
        c.execute("UPDATE jobs SET pending_action=NULL WHERE id=?1", [id])?;
        return Ok(false);
    }
    transaction(c, || {
        if action == "defer" {
            defer_record(
                c,
                home,
                &job,
                job.hold_reason.as_deref().unwrap_or("Deferred"),
            )?;
        } else {
            c.execute(
                "UPDATE jobs SET state='cancelled',pending_action=NULL WHERE id=?1",
                [id],
            )?;
        }
        c.execute(
            "UPDATE jobs SET exit_code=?1 WHERE id=?2",
            params![code, id],
        )?;
        Ok(Value::Null)
    })?;
    Ok(true)
}

fn clear_records(
    c: &Connection,
    queues: &[String],
    history_only: bool,
    delete: bool,
    acknowledge: bool,
) -> Result<Value> {
    for queue in queues {
        if c.query_row(
            "SELECT COUNT(*) FROM queues WHERE name=?1 AND archived=0",
            [queue],
            |row| row.get::<_, i64>(0),
        )? == 0
        {
            bail!("unknown queue");
        }
    }
    let selected: HashSet<&String> = queues.iter().collect();
    let current = jobs(c)?
        .into_iter()
        .filter(|job| selected.contains(&job.queue) && !job.archived)
        .collect::<Vec<_>>();
    if (!history_only || delete)
        && current
            .iter()
            .any(|job| job.state == "running" || job.callback_state == "running")
    {
        bail!("stop/defer active work before clearing/deleting queues; nothing was cleared");
    }
    if current
        .iter()
        .any(|job| matches!(job.state.as_str(), "failed" | "interrupted"))
        && !acknowledge
    {
        bail!("inspect failed/interrupted effects and explicitly acknowledge them before clearing");
    }
    let mut count = 0;
    for job in current {
        if history_only
            && (!crate::terminal(&job.state)
                || job.state == "deferred"
                || job.callback_state == "running")
        {
            continue;
        }
        let state = if matches!(job.state.as_str(), "failed" | "interrupted") {
            "resolved"
        } else if matches!(job.state.as_str(), "queued" | "deferred") {
            "cancelled"
        } else {
            &job.state
        };
        c.execute(
            "UPDATE jobs SET archived=1,state=?1 WHERE id=?2",
            params![state, job.id],
        )?;
        count += 1;
    }
    if delete {
        for queue in queues {
            c.execute("UPDATE queues SET archived=1 WHERE name=?1", [queue])?;
        }
    }
    Ok(json!({"queues":queues,"cleared":count,"deleted":delete,"deduplication_preserved":true}))
}
