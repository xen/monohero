//! Explicit user-authorized lifecycle changes; no inference sessions.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub(crate) struct Rpc {
    child: Child,
    lines: mpsc::Receiver<std::io::Result<String>>,
    id: u64,
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Rpc {
    pub(crate) fn start(codex: &Path) -> Result<Self> {
        Self::connect(codex, false, None)
    }
    pub(crate) fn connect(codex: &Path, proxy: bool, socket: Option<&Path>) -> Result<Self> {
        let mut command = Command::new(codex);
        command.args(if proxy {
            vec!["app-server", "proxy"]
        } else {
            vec!["app-server", "--stdio"]
        });
        if let Some(socket) = socket {
            if !proxy {
                bail!("a control socket requires explicit proxy mode");
            }
            command.arg("--sock").arg(socket);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().context("missing RPC output")?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut rpc = Self {
            child,
            lines,
            id: 0,
        };
        rpc.request("initialize", json!({"clientInfo":{"name":"monohero-goals","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        writeln!(
            rpc.child.stdin.as_mut().context("missing RPC input")?,
            "{}",
            json!({"jsonrpc":"2.0","method":"initialized"})
        )?;
        Ok(rpc)
    }
    pub(crate) fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let stdin = self.child.stdin.as_mut().context("missing RPC input")?;
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params})
        )?;
        stdin.flush()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("goal RPC timeout")?;
            let line = self
                .lines
                .recv_timeout(remaining)
                .context("goal RPC disconnected or timed out")??;
            let response: Value = serde_json::from_str(&line)?;
            if response["id"] != self.id {
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!("goal RPC {method}: {error}");
            }
            return response
                .get("result")
                .cloned()
                .context("invalid goal RPC result");
        }
    }
    fn get(&mut self, thread: &str) -> Result<Value> {
        let response = self.request("thread/goal/get", json!({"threadId":thread}))?;
        let goal = response.get("goal").context("missing goal field")?;
        if goal.is_null() {
            bail!("thread has no goal");
        }
        Ok(goal.clone())
    }
    fn set(&mut self, thread: &str, status: &str) -> Result<Value> {
        let response = self.request(
            "thread/goal/set",
            json!({"threadId":thread,"status":status}),
        )?;
        response
            .get("goal")
            .cloned()
            .context("missing updated goal")
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    queue: String,
    key: String,
    state: String,
    goal: Value,
}
fn save(path: &Path, record: &Saved) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, record)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}
fn paths(home: &Path, thread: &str) -> Result<(PathBuf, std::fs::File)> {
    let directory = home.join("goals");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(format!("{thread}.lock")))?;
    lock.lock_exclusive()?;
    Ok((directory.join(format!("{thread}.json")), lock))
}
fn unchanged(current: &Value, saved: &Value) -> bool {
    ["threadId", "objective", "createdAt", "updatedAt"]
        .iter()
        .all(|field| current.get(field).is_some() && current.get(field) == saved.get(field))
        && current.get("tokenBudget") == saved.get("tokenBudget")
}

pub fn park(home: &Path, codex: &Path, thread: &str, queue: &str, key: &str) -> Result<()> {
    let (path, _lock) = paths(home, thread)?;
    let mut rpc = Rpc::start(codex)?;
    let current = rpc.get(thread)?;
    if path.exists() {
        let old: Saved = serde_json::from_slice(&fs::read(&path)?)?;
        if old.state != "restored" {
            if old.state == "parked"
                && old.queue == queue
                && old.key == key
                && current["status"] == "paused"
                && unchanged(&current, &old.goal)
            {
                return Ok(());
            }
            bail!("an existing goal wait requires inspection; do not overwrite it");
        }
    }
    if current["status"] != "active" {
        bail!("only an existing active goal can be parked");
    }
    let mut record = Saved {
        queue: queue.into(),
        key: key.into(),
        state: "parking".into(),
        goal: current.clone(),
    };
    save(&path, &record)?;
    let paused = rpc.set(thread, "paused")?;
    if paused["status"] != "paused"
        || [
            "threadId",
            "objective",
            "tokenBudget",
            "createdAt",
            "tokensUsed",
            "timeUsedSeconds",
        ]
        .iter()
        .any(|field| paused.get(field) != current.get(field))
    {
        bail!("goal pause did not preserve identity/accounting; inspect saved wait");
    }
    record.state = "parked".into();
    record.goal = paused;
    save(&path, &record)
}

pub fn restore(home: &Path, codex: &Path, thread: &str, owner: Option<(&str, &str)>) -> Result<()> {
    let (path, _lock) = paths(home, thread)?;
    let mut record: Saved =
        serde_json::from_slice(&fs::read(&path).context("no saved goal wait")?)?;
    if record.state != "parked" {
        bail!("goal wait is not parked; inspect it before recovery");
    }
    if let Some((queue, key)) = owner {
        if record.queue != queue || record.key != key {
            bail!("job does not own this goal wait");
        }
    }
    if owner.is_none() {
        let jobs = super::call(
            home,
            &super::Request::Status {
                queue: Some(record.queue.clone()),
            },
        )?;
        if let Some(job) = jobs
            .as_array()
            .context("invalid jobs")?
            .iter()
            .find(|job| job["key"] == record.key)
        {
            let rows = jobs.as_array().context("invalid jobs")?;
            let mut owned =
                std::collections::HashSet::from([job["id"].as_i64().context("invalid job ID")?]);
            loop {
                let old_len = owned.len();
                for candidate in rows {
                    if candidate["parent_id"]
                        .as_i64()
                        .is_some_and(|parent| owned.contains(&parent))
                    {
                        owned.insert(candidate["id"].as_i64().context("invalid successor ID")?);
                    }
                }
                if owned.len() == old_len {
                    break;
                }
            }
            for candidate in rows.iter().filter(|candidate| {
                candidate["id"]
                    .as_i64()
                    .is_some_and(|id| owned.contains(&id))
            }) {
                let replaced = candidate["state"] == "requeued"
                    && rows.iter().any(|next| next["parent_id"] == candidate["id"]);
                if !replaced
                    && !matches!(
                        candidate["state"].as_str(),
                        Some("cancelled" | "failed" | "interrupted" | "resolved")
                    )
                {
                    bail!("cancel or inspect the original job and every live successor before explicitly unparking its goal");
                }
            }
        }
    }
    let mut rpc = Rpc::start(codex)?;
    let current = rpc.get(thread)?;
    if current["status"] != "paused" || !unchanged(&current, &record.goal) {
        bail!("goal changed while waiting; automatic restoration refused");
    }
    // Durable intent makes uncertain RPC delivery visible and prevents blind retries.
    record.state = "restoring".into();
    save(&path, &record)?;
    let restored = rpc.set(thread, "active")?;
    if restored["status"] != "active"
        || [
            "threadId",
            "objective",
            "tokenBudget",
            "createdAt",
            "tokensUsed",
            "timeUsedSeconds",
        ]
        .iter()
        .any(|field| restored.get(field) != current.get(field))
    {
        bail!("goal restoration did not preserve identity/accounting; inspect saved wait");
    }
    record.state = "restored".into();
    record.goal = restored;
    save(&path, &record)
}
