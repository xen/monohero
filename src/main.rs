#![cfg(unix)]
mod chat;
mod dashboard;
mod fence;
mod goal;
mod management;
mod monitor;
mod view;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use fs2::FileExt;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Shared private state directory (must be identical across worktrees).
    #[arg(long, env = "MONOHERO_HOME", global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Run the local queue service in the foreground.
    Serve,
    /// Persist a job and return immediately. No implicit shell expansion.
    Submit {
        #[arg(long)]
        queue: String,
        #[arg(long)]
        key: String,
        /// Working directory for this job; defaults to the submitting directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Absolute executable invoked once after completion with job ID and state.
        #[arg(long)]
        on_complete: Option<PathBuf>,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Read one snapshot. Never call in a polling loop.
    Status {
        #[arg(long)]
        queue: Option<String>,
    },
    /// Show queues in FIFO order as a readable, one-time snapshot.
    Queue {
        /// Show only this project's queue.
        #[arg(long)]
        queue: Option<String>,
        /// Include completed, cancelled and resolved jobs.
        #[arg(long)]
        all: bool,
        /// Print the selected jobs as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Block on a socket until a job reaches a terminal state.
    Wait { id: i64 },
    /// Acknowledge inspection of a failed/interrupted job.
    Resolve { id: i64 },
    /// Cancel a queued job. Running jobs cannot be cancelled.
    Cancel { id: i64 },
    #[command(flatten)]
    Management(management::Actions),
    /// Open the local queue dashboard.
    Dashboard {
        /// Start with only the macOS menu-bar icon.
        #[arg(long)]
        hidden: bool,
        /// Explicit path to the separately installed GPUI Kit UI executable.
        #[arg(long)]
        ui: Option<PathBuf>,
    },
    /// Print agent integration instructions.
    Guide,
    #[command(flatten)]
    Chat(chat::Actions),
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum Request {
    Submit {
        queue: String,
        key: String,
        cwd: PathBuf,
        command: Vec<String>,
        callback: Option<PathBuf>,
    },
    Status {
        queue: Option<String>,
    },
    Snapshot,
    Manage {
        action: management::Control,
    },
    Wait {
        id: i64,
    },
    Resolve {
        id: i64,
    },
    Cancel {
        id: i64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    id: i64,
    queue: String,
    key: String,
    cwd: PathBuf,
    command: Vec<String>,
    state: String,
    exit_code: Option<i32>,
    log: PathBuf,
    callback: Option<PathBuf>,
    callback_state: String,
    #[serde(default)]
    position: i64,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    parent_id: Option<i64>,
    #[serde(default)]
    hold_reason: Option<String>,
    #[serde(default)]
    pending_action: Option<String>,
    #[serde(default)]
    observation: Option<serde_json::Value>,
}
struct Shared {
    home: PathBuf,
    generation: Mutex<u64>,
    changed: Condvar,
    monitor_started: Arc<std::sync::atomic::AtomicBool>,
}
fn db(home: &Path) -> Result<Connection> {
    let c = Connection::open(home.join("jobs.sqlite3"))?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(c)
}
fn jobs(c: &Connection) -> Result<Vec<Job>> {
    let mut s = c.prepare("SELECT id,queue,key,cwd,command,state,exit_code,callback,callback_state,position,archived,parent_id,hold_reason,pending_action,observation FROM jobs ORDER BY position,id")?;
    let rows = s.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<i32>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, bool>(10)?,
            r.get::<_, Option<i64>>(11)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, Option<String>>(13)?,
            r.get::<_, Option<String>>(14)?,
        ))
    })?;
    rows.map(|row| {
        let (
            id,
            queue,
            key,
            cwd,
            command,
            state,
            exit_code,
            callback,
            callback_state,
            position,
            archived,
            parent_id,
            hold_reason,
            pending_action,
            observation,
        ) = row?;
        Ok(Job {
            id,
            queue,
            key,
            cwd: cwd.into(),
            command: serde_json::from_str(&command)?,
            state,
            exit_code,
            log: PathBuf::new(),
            callback: callback.map(Into::into),
            callback_state,
            position,
            archived,
            parent_id,
            hold_reason,
            pending_action,
            observation: observation
                .map(|body| serde_json::from_str(&body))
                .transpose()?,
        })
    })
    .collect()
}
fn terminal(s: &str) -> bool {
    matches!(
        s,
        "succeeded" | "failed" | "interrupted" | "cancelled" | "resolved" | "deferred" | "requeued"
    )
}
fn signal(s: &Shared) {
    let mut g = s.generation.lock().unwrap();
    *g += 1;
    s.changed.notify_all();
}
fn name(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 200 || value.chars().any(char::is_control) {
        bail!("queue and key must be 1–200 characters without control characters");
    }
    Ok(())
}
fn handle(r: Request, s: &Shared) -> Result<serde_json::Value> {
    let c = db(&s.home)?;
    match r {
        Request::Submit {
            queue,
            key,
            cwd,
            command,
            callback,
        } => {
            name(&queue)?;
            name(&key)?;
            if !cwd.is_absolute() || !cwd.is_dir() || command.is_empty() {
                bail!("absolute existing cwd and nonempty command required");
            }
            if callback
                .as_ref()
                .is_some_and(|p| !p.is_absolute() || !p.is_file())
            {
                bail!("callback must be an absolute existing executable path");
            }
            c.execute(
                "INSERT INTO queues(name) VALUES(?1) ON CONFLICT(name) DO UPDATE SET archived=0",
                [&queue],
            )?;
            let encoded = serde_json::to_string(&command)?;
            // Immutable deduplication key: a retry may not silently replace a workflow.
            c.execute("INSERT INTO jobs(queue,key,cwd,command,state,callback,callback_state,position) VALUES(?1,?2,?3,?4,'queued',?5,'none',(SELECT COALESCE(MAX(position),0)+1 FROM jobs WHERE queue=?1)) ON CONFLICT(queue,key) DO NOTHING",params![queue,key,cwd.to_str().context("non UTF-8 cwd")?,encoded,callback.as_ref().and_then(|p|p.to_str())])?;
            let j = jobs(&c)?
                .into_iter()
                .find(|j| j.queue == queue && j.key == key)
                .unwrap();
            if j.cwd != cwd || j.command != command || j.callback != callback {
                bail!("key already exists with different inputs; choose a new key");
            }
            signal(s);
            Ok(serde_json::json!({"id":j.id,"queue":queue,"state":j.state}))
        }
        Request::Snapshot => management::snapshot(s),
        Request::Manage { action } => management::apply(action, s),
        Request::Status { queue } => {
            let mut js = jobs(&c)?;
            js.retain(|j| queue.as_ref().map_or(true, |q| *q == j.queue));
            for j in &mut js {
                j.log = s.home.join(format!("{}.log", j.id));
            }
            Ok(serde_json::to_value(js)?)
        }
        Request::Wait { id } => {
            let mut generation = s.generation.lock().unwrap();
            loop {
                let mut j = jobs(&c)?
                    .into_iter()
                    .find(|j| j.id == id)
                    .context("unknown job")?;
                if terminal(&j.state) {
                    j.log = s.home.join(format!("{}.log", id));
                    return Ok(serde_json::to_value(j)?);
                }
                generation = s.changed.wait(generation).unwrap();
            }
        }
        Request::Resolve { id } => {
            if c.execute("UPDATE jobs SET state='resolved' WHERE id=?1 AND state IN ('failed','interrupted')",[id])?!=1 { bail!("only failed/interrupted jobs can be resolved"); }
            signal(s);
            Ok(serde_json::json!({"id":id,"state":"resolved"}))
        }
        Request::Cancel { id } => {
            if c.execute(
                "UPDATE jobs SET state='cancelled' WHERE id=?1 AND state='queued'",
                [id],
            )? != 1
            {
                bail!("only queued jobs can be cancelled");
            }
            signal(s);
            Ok(serde_json::json!({"id":id,"state":"cancelled"}))
        }
    }
}
fn execute(id: i64, s: &Shared, guard: &File) -> Result<()> {
    let c = db(&s.home)?;
    let j = jobs(&c)?
        .into_iter()
        .find(|j| j.id == id)
        .context("missing job")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(s.home.join(format!("{id}.log")))?;
    // Keep daemon ownership locked if the daemon dies while a child survives.
    // Child inherits this descriptor; do not use close_fds wrappers around it.
    use std::os::fd::AsRawFd;
    let inherited = guard.try_clone()?;
    let fd = inherited.as_raw_fd();
    unsafe {
        if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let mut command = j.command.clone();
    // Operators may explicitly adopt queued workers from a known older installation.
    // Preserve durable submission arguments and IDs, including saved goal wait keys.
    if command.get(3).map(String::as_str) == Some("chat-worker") {
        let adoptions: std::collections::BTreeMap<PathBuf, PathBuf> =
            match fs::read(s.home.join("worker-adoptions.json")) {
                Ok(body) => serde_json::from_slice(&body)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
                Err(e) => return Err(e.into()),
            };
        if let Some(target) = adoptions.get(&fs::canonicalize(&command[0])?) {
            let argument = |name: &str| {
                command[4..]
                    .windows(2)
                    .find(|pair| pair[0] == name)
                    .map(|pair| pair[1].as_str())
            };
            if command[1] != "--home"
                || fs::canonicalize(&command[2])? != fs::canonicalize(&s.home)?
                || argument("--queue") != Some(j.queue.as_str())
                || argument("--key") != Some(j.key.as_str())
            {
                anyhow::bail!("worker adoption refused: submitted native worker identity does not match its job");
            }
            command[0] = target
                .to_str()
                .context("worker executable is not UTF-8")?
                .into();
            if !command.iter().any(|arg| arg == "--managed-commands") {
                command.push("--managed-commands".into());
            }
        }
    }
    // Future grants using this updated executable adopt fencing; persisted inputs stay immutable.
    if command.get(3).map(String::as_str) == Some("chat-worker")
        && !command.iter().any(|arg| arg == "--managed-commands")
        && fs::canonicalize(&command[0])? == fs::canonicalize(std::env::current_exe()?)?
    {
        command.push("--managed-commands".into());
    }
    // Managed chat workers use the same fence for their own protected commands.
    let work = if command.get(3).map(String::as_str) == Some("chat-worker") {
        None
    } else {
        Some(fence::inherited_work(&s.home, id)?)
    };
    use std::os::unix::process::CommandExt;
    let mut process = Command::new(&command[0]);
    process.args(&command[1..]);
    if let Some(file) = &work {
        let fd = file.as_raw_fd();
        unsafe {
            process.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let status = process
        .current_dir(&j.cwd)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .status();
    drop(work);
    // Descendants can outlive the command itself; do not grant the next job
    // until all inherited work locks have drained.
    let drained = fence::exclusive(&s.home, id)?;
    drop(drained);
    let (state, code) = match status {
        Ok(v) if v.success() => ("succeeded", v.code()),
        Ok(v) => ("failed", v.code()),
        Err(e) => {
            writeln!(&log, "Unable to execute job: {e}")?;
            ("failed", None)
        }
    };
    if management::complete_release(&c, &s.home, id, code)? {
        signal(s);
        return Ok(());
    }
    c.execute(
        "UPDATE jobs SET state=?1,exit_code=?2,pending_action=NULL WHERE id=?3",
        params![state, code, id],
    )?;
    signal(s);
    if let Some(callback) = j.callback {
        c.execute("UPDATE jobs SET callback_state='running' WHERE id=?1", [id])?;
        let callback_work = fence::inherited_work(&s.home, id)?;
        let callback_fd = callback_work.as_raw_fd();
        let mut process = Command::new(callback);
        unsafe {
            process.pre_exec(move || {
                if libc::fcntl(callback_fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let result = process
            .arg(id.to_string())
            .arg(state)
            .current_dir(&j.cwd)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status();
        drop(callback_work);
        drop(fence::exclusive(&s.home, id)?);
        let callback_state = if result.is_ok_and(|v| v.success()) {
            "succeeded"
        } else {
            "failed"
        };
        c.execute(
            "UPDATE jobs SET callback_state=?1 WHERE id=?2",
            params![callback_state, id],
        )?;
        signal(s);
    }
    drop(inherited);
    Ok(())
}
fn worker(queue: String, s: Arc<Shared>, guard: File) -> Result<()> {
    let c = db(&s.home)?;
    let mut generation = s.generation.lock().unwrap();
    loop {
        // One worker per queue. Transaction makes cancellation and claiming atomic.
        c.execute_batch("BEGIN IMMEDIATE")?;
        let js = jobs(&c)?;
        let paused: bool =
            c.query_row("SELECT paused FROM queues WHERE name=?1", [&queue], |r| {
                r.get(0)
            })?;
        let next = if paused {
            None
        } else {
            js.iter()
                .find(|j| {
                    j.queue == queue
                        && j.state == "queued"
                        && j.hold_reason.is_none()
                        && !j.archived
                })
                .map(|j| j.id)
        };
        if let Some(id) = next {
            c.execute("UPDATE jobs SET state='running' WHERE id=?1", [id])?;
        }
        c.execute_batch("COMMIT")?;
        if let Some(id) = next {
            drop(generation);
            signal(&s);
            if let Err(e) = execute(id, &s, &guard) {
                eprintln!("worker {queue}: {e:#}");
                c.execute("UPDATE jobs SET state='interrupted' WHERE id=?1", [id])?;
                signal(&s);
            }
            generation = s.generation.lock().unwrap();
        } else {
            generation = s.changed.wait(generation).unwrap();
        }
    }
}
fn serve(home: PathBuf) -> Result<()> {
    fs::create_dir_all(&home)?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
    let guard = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("daemon.lock"))?;
    guard
        .try_lock_exclusive()
        .context("daemon or surviving job already owns this home")?;
    let active_work = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("active-work.lock"))?;
    active_work
        .try_lock_exclusive()
        .context("protected children still own this home; do not restart over them")?;
    let c = db(&home)?;
    c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY AUTOINCREMENT,queue TEXT NOT NULL,key TEXT NOT NULL,cwd TEXT NOT NULL,command TEXT NOT NULL,state TEXT NOT NULL,exit_code INTEGER,callback TEXT,callback_state TEXT NOT NULL,UNIQUE(queue,key)); UPDATE jobs SET state='interrupted' WHERE state='running'; UPDATE jobs SET callback_state='interrupted' WHERE callback_state='running';")?;
    management::migrate(&c)?;
    let socket = home.join("control.sock");
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let s = Arc::new(Shared {
        home,
        generation: Mutex::new(0),
        changed: Condvar::new(),
        monitor_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let workers = Arc::new(Mutex::new(HashSet::<String>::new()));
    // Manager sleeps on committed submission/completion events, never a timer.
    let manager_s = s.clone();
    let manager_workers = workers.clone();
    let manager_guard = guard.try_clone()?;
    thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut generation = manager_s.generation.lock().unwrap();
            loop {
                for j in jobs(&db(&manager_s.home)?)?
                    .into_iter()
                    .filter(|job| !job.archived)
                {
                    if manager_workers.lock().unwrap().insert(j.queue.clone()) {
                        let shared = manager_s.clone();
                        let g = manager_guard.try_clone()?;
                        thread::spawn(move || {
                            if let Err(e) = worker(j.queue, shared, g) {
                                eprintln!("queue worker stopped: {e:#}");
                                std::process::exit(1);
                            }
                        });
                    }
                }
                generation = manager_s.changed.wait(generation).unwrap();
            }
        })();
        if let Err(e) = result {
            eprintln!("queue scheduler stopped: {e:#}");
            std::process::exit(1);
        }
    });
    FileExt::unlock(&active_work)?;
    monitor::start_configured(&s.home, s.monitor_started.clone())?;
    eprintln!("monohero ready: {}", socket.display());
    for stream in listener.incoming() {
        let mut stream = stream?;
        let shared = s.clone();
        thread::spawn(move || {
            let result = (|| -> Result<serde_json::Value> {
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request = serde_json::from_str(&line)?;
                handle(request, &shared)
            })();
            let response = match result {
                Ok(data) => serde_json::json!({"ok":true,"data":data}),
                Err(e) => serde_json::json!({"ok":false,"error":format!("{e:#}")}),
            };
            let _ = writeln!(stream, "{response}");
        });
    }
    Ok(())
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Action::Guide) {
        print!("{}", include_str!("../integrations/AGENT.md"));
        return Ok(());
    }
    let home = cli.home.unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").expect("set HOME or --home"))
            .join(".local/state/monohero")
    });
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    let request = match cli.command {
        Action::Serve => return serve(home),
        Action::Chat(action) => return chat::run(action, &home),
        Action::Management(action) => return management::run(action, &home),
        Action::Dashboard { hidden, ui } => return dashboard::launch(&home, hidden, ui),
        Action::Queue { queue, all, json } => return view::show(&home, queue, all, json),
        Action::Submit {
            queue,
            key,
            cwd,
            on_complete,
            command,
        } => Request::Submit {
            queue,
            key,
            cwd: fs::canonicalize(cwd.unwrap_or(std::env::current_dir()?))?,
            command,
            callback: on_complete,
        },
        Action::Status { queue } => Request::Status { queue },
        Action::Wait { id } => Request::Wait { id },
        Action::Resolve { id } => Request::Resolve { id },
        Action::Cancel { id } => Request::Manage {
            action: management::Control::Cancel { id },
        },
        Action::Guide => unreachable!(),
    };
    let waiting = matches!(request, Request::Wait { .. });
    let data = call(&home, &request)?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    if waiting && data["state"] != "succeeded" {
        std::process::exit(1);
    }
    Ok(())
}

fn call(home: &Path, request: &Request) -> Result<serde_json::Value> {
    let mut stream = UnixStream::connect(home.join("control.sock"))
        .context("cannot reach Monohero; start `monohero serve` with the same --home")?;
    writeln!(stream, "{}", serde_json::to_string(request)?)?;
    if !matches!(request, Request::Wait { .. }) {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: serde_json::Value = serde_json::from_str(&line)
        .context("daemon disconnected; inspect durable state before retrying")?;
    if response["ok"] != true {
        bail!("{}", response["error"]);
    }
    Ok(response["data"].clone())
}
