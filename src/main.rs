#![cfg(unix)]
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
    /// Block on a socket until a job reaches a terminal state.
    Wait { id: i64 },
    /// Acknowledge inspection of a failed/interrupted job; unblock its queue.
    Resolve { id: i64 },
    /// Cancel a queued job. Running jobs cannot be cancelled.
    Cancel { id: i64 },
    /// Print agent integration instructions.
    Guide,
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
#[derive(Debug, Serialize, Deserialize)]
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
}
struct Shared {
    home: PathBuf,
    generation: Mutex<u64>,
    changed: Condvar,
}
fn db(home: &Path) -> Result<Connection> {
    let c = Connection::open(home.join("jobs.sqlite3"))?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(c)
}
fn jobs(c: &Connection) -> Result<Vec<Job>> {
    let mut s = c.prepare("SELECT id,queue,key,cwd,command,state,exit_code,callback,callback_state FROM jobs ORDER BY id")?;
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
        ))
    })?;
    rows.map(|row| {
        let (id, queue, key, cwd, command, state, exit_code, callback, callback_state) = row?;
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
        })
    })
    .collect()
}
fn terminal(s: &str) -> bool {
    matches!(
        s,
        "succeeded" | "failed" | "interrupted" | "cancelled" | "resolved"
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
            let encoded = serde_json::to_string(&command)?;
            // Immutable deduplication key: a retry may not silently replace a workflow.
            c.execute("INSERT INTO jobs(queue,key,cwd,command,state,callback,callback_state) VALUES(?1,?2,?3,?4,'queued',?5,'none') ON CONFLICT(queue,key) DO NOTHING",params![queue,key,cwd.to_str().context("non UTF-8 cwd")?,encoded,callback.as_ref().and_then(|p|p.to_str())])?;
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
    let status = Command::new(&j.command[0])
        .args(&j.command[1..])
        .current_dir(&j.cwd)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .status();
    let (state, code) = match status {
        Ok(v) if v.success() => ("succeeded", v.code()),
        Ok(v) => ("failed", v.code()),
        Err(e) => {
            writeln!(&log, "Unable to execute job: {e}")?;
            ("failed", None)
        }
    };
    c.execute(
        "UPDATE jobs SET state=?1,exit_code=?2 WHERE id=?3",
        params![state, code, id],
    )?;
    signal(s);
    if let Some(callback) = j.callback {
        c.execute("UPDATE jobs SET callback_state='running' WHERE id=?1", [id])?;
        let result = Command::new(callback)
            .arg(id.to_string())
            .arg(state)
            .current_dir(&j.cwd)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status();
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
        let blocked = js
            .iter()
            .any(|j| j.queue == queue && matches!(j.state.as_str(), "failed" | "interrupted"));
        let next = if blocked {
            None
        } else {
            js.iter()
                .find(|j| j.queue == queue && j.state == "queued")
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
    let c = db(&home)?;
    c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY AUTOINCREMENT,queue TEXT NOT NULL,key TEXT NOT NULL,cwd TEXT NOT NULL,command TEXT NOT NULL,state TEXT NOT NULL,exit_code INTEGER,callback TEXT,callback_state TEXT NOT NULL,UNIQUE(queue,key)); UPDATE jobs SET state='interrupted' WHERE state='running'; UPDATE jobs SET callback_state='interrupted' WHERE callback_state='running';")?;
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
                for j in jobs(&db(&manager_s.home)?)? {
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
        Action::Cancel { id } => Request::Cancel { id },
        Action::Guide => unreachable!(),
    };
    let waiting = matches!(request, Request::Wait { .. });
    let mut stream = UnixStream::connect(home.join("control.sock"))
        .context("cannot reach Monohero; explicitly start `monohero serve` with the same --home")?;
    writeln!(stream, "{}", serde_json::to_string(&request)?)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: serde_json::Value = serde_json::from_str(&line)
        .context("daemon disconnected; inspect durable job state after restart")?;
    if response["ok"] != true {
        bail!("{}", response["error"]);
    }
    println!("{}", serde_json::to_string_pretty(&response["data"])?);
    if waiting && response["data"]["state"] != "succeeded" {
        std::process::exit(1);
    }
    Ok(())
}
