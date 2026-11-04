//! Event-driven chat handoff on the stable command/wait daemon protocol.
use super::{call, name, Request};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Subcommand)]
pub enum Actions {
    /// Queue an authorized existing chat; send a message when its slot is granted.
    Enqueue {
        #[arg(long)]
        queue: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        thread: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long, default_value = "codex")]
        codex: PathBuf,
        #[arg(long, default_value = "Continue the previously authorized task.")]
        message: String,
        /// Explicitly authorize pausing this active goal until its queued grant.
        #[arg(long)]
        park_goal: bool,
    },
    /// Configure a native URL opener to wake unloaded Codex app chats after delivery.
    ConfigureWake {
        /// Absolute executable, e.g. /usr/bin/open on macOS; receives one Codex URL.
        #[arg(long)]
        opener: PathBuf,
    },
    /// Explicitly restore an unchanged goal left paused after cancellation/failure.
    UnparkGoal {
        #[arg(long)]
        thread: String,
        #[arg(long, default_value = "codex")]
        codex: PathBuf,
    },
    /// Confirm in the destination chat that it received its queue grant.
    Claim {
        id: i64,
        #[arg(long)]
        thread: String,
    },
    /// Finish all protected work and release the chat's slot.
    Finish {
        id: i64,
        #[arg(long)]
        thread: String,
        #[arg(long)]
        failed: bool,
    },
    /// Confirm delivery of a completion notice; this does not grant a release slot.
    Acknowledge {
        id: i64,
        #[arg(long)]
        thread: String,
    },
    /// Message a command job's authorized owner when that job becomes terminal.
    Notify {
        id: i64,
        #[arg(long)]
        thread: String,
        #[arg(long, default_value = "codex")]
        codex: PathBuf,
    },
    #[command(hide = true)]
    ChatWorker {
        #[arg(long)]
        queue: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        thread: String,
        #[arg(long)]
        codex: PathBuf,
        #[arg(long)]
        message: String,
        /// Explicitly authorize pausing this active goal until its queued grant.
        #[arg(long)]
        park_goal: bool,
        #[arg(long)]
        managed_commands: bool,
        #[arg(long)]
        batch_requests: bool,
        #[arg(long)]
        batch_join_only: bool,
        #[arg(long)]
        goal_key: Option<String>,
    },
    #[command(hide = true)]
    NotifyWorker {
        id: i64,
        #[arg(long)]
        thread: String,
        #[arg(long)]
        codex: PathBuf,
    },
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    job_id: i64,
    thread_id: String,
    kind: String,
    state: String,
    message_id: Option<String>,
    claimed_at: Option<u64>,
    updated_at: u64,
    finish_requested: Option<bool>,
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_wake: Option<String>,
    #[serde(default)]
    managed: bool,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
pub(crate) fn validate_thread(id: &str) -> Result<()> {
    if id.len() != 36
        || !id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit() && !b.is_ascii_uppercase()
            }
        })
    {
        bail!("use an exact canonical lowercase Codex thread UUID, not a name or --last");
    }
    Ok(())
}
fn text(path: &Path) -> Result<String> {
    Ok(path.to_str().context("non UTF-8 path")?.to_owned())
}
pub(crate) fn executable(path: &Path) -> Result<PathBuf> {
    let found = if path.components().count() > 1 || path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::split_paths(&std::env::var_os("PATH").context("PATH is unset")?)
            .map(|dir| dir.join(path))
            .find(|p| p.is_file())
            .context("Codex executable not found on PATH")?
    };
    let found = fs::canonicalize(found)?;
    if !found.is_file() || fs::metadata(&found)?.permissions().mode() & 0o111 == 0 {
        bail!("Codex path is not an executable file");
    }
    Ok(found)
}
fn private_dir(home: &Path) -> Result<PathBuf> {
    let p = home.join("chats");
    fs::create_dir_all(&p)?;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
    Ok(p)
}
fn persist(path: &Path, r: &mut Receipt) -> Result<()> {
    r.updated_at = now();
    let tmp = path.with_extension("json.tmp");
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    serde_json::to_writer_pretty(&mut f, r)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
fn print(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn own_command(home: &Path, args: Vec<String>) -> Result<Vec<String>> {
    let mut v = vec![
        text(&std::env::current_exe()?)?,
        "--home".into(),
        text(home)?,
    ];
    v.extend(args);
    Ok(v)
}
fn submit(
    home: &Path,
    queue: String,
    key: String,
    cwd: PathBuf,
    command: Vec<String>,
) -> Result<Value> {
    call(
        home,
        &Request::Submit {
            queue,
            key,
            cwd,
            command,
            callback: None,
        },
    )
}
fn observer_queue(id: i64, thread: &str) -> String {
    format!("monohero-notify-{id}-{thread}")
}
fn running_job(home: &Path, queue: &str, key: &str) -> Result<Value> {
    let jobs = call(
        home,
        &Request::Status {
            queue: Some(queue.into()),
        },
    )?;
    Ok(jobs
        .as_array()
        .context("invalid jobs")?
        .iter()
        .find(|j| j["key"] == key && j["state"] == "running")
        .context("internal chat worker must be started by its running Monohero job")?
        .clone())
}
fn reply(home: &Path, id: i64, thread: &str, op: &str, failed: bool) -> Result<()> {
    validate_thread(thread)?;
    let mut s = UnixStream::connect(home.join("chats").join(format!("{id}.sock")))
        .context("no active chat socket; inspect job state, do not force-release")?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    writeln!(s, "{}", json!({"thread":thread,"op":op,"failed":failed}))?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line)?;
    if response["ok"] != true {
        bail!("{}", response["error"]);
    }
    print(response)
}
pub fn run(action: Actions, home: &Path) -> Result<()> {
    match action {
        Actions::Enqueue {
            queue,
            key,
            thread,
            cwd,
            codex,
            message,
            park_goal,
        } => {
            validate_thread(&thread)?;
            name(&queue)?;
            name(&key)?;
            let codex = executable(&codex)?;
            let cwd = fs::canonicalize(cwd.unwrap_or(std::env::current_dir()?))?;
            let _admission = crate::batch::admission(home, &queue, &thread)?;
            let supported = crate::batch::capable(home)?;
            let mut command = own_command(
                home,
                vec![
                    "chat-worker".into(),
                    "--queue".into(),
                    queue.clone(),
                    "--key".into(),
                    key.clone(),
                    "--thread".into(),
                    thread.clone(),
                    "--codex".into(),
                    text(&codex)?,
                    "--message".into(),
                    message,
                ],
            )?;
            command.push("--managed-commands".into());
            command.push("--batch-requests".into());
            if supported {
                if let Some((old_cwd, old_command)) = crate::batch::lookup(home, &queue, &key)? {
                    let same = old_cwd == text(&cwd)?
                        && old_command.first() == command.first()
                        && crate::batch::argument(&old_command, "--thread")
                            == Some(thread.as_str())
                        && crate::batch::argument(&old_command, "--codex")
                            == Some(text(&codex)?.as_str())
                        && crate::batch::argument(&old_command, "--message")
                            == crate::batch::argument(&command, "--message")
                        && old_command.iter().any(|arg| arg == "--park-goal") == park_goal;
                    if !same {
                        bail!("key already exists with different inputs; choose a new key");
                    }
                    return print(submit(home, queue, key, cwd, old_command)?);
                }
            } else {
                let status = call(
                    home,
                    &Request::Status {
                        queue: Some(queue.clone()),
                    },
                )?;
                for value in status.as_array().context("invalid job list")? {
                    let job: crate::Job = serde_json::from_value(value.clone())?;
                    if job.key == key
                        || job.archived
                        || !matches!(job.state.as_str(), "queued" | "running" | "deferred")
                        || crate::management::thread_id(&job).as_deref() != Some(thread.as_str())
                        || job.command.get(3).map(String::as_str) != Some("chat-worker")
                    {
                        continue;
                    }
                    let claimed = crate::management::receipt(home, job.id)?
                        .is_some_and(|receipt| !receipt["claimed_at"].is_null());
                    if !claimed {
                        bail!("the running Monohero service cannot accumulate this chat's pending requests; update the service after its active work finishes");
                    }
                }
            }
            let mut goal_already_dispatched = false;
            if park_goal {
                let all = call(home, &Request::Status { queue: None })?;
                for value in all.as_array().context("invalid job list")? {
                    let job: crate::Job = serde_json::from_value(value.clone())?;
                    if job.archived
                        || job.state != "running"
                        || crate::management::thread_id(&job).as_deref() != Some(thread.as_str())
                        || job.command.get(3).map(String::as_str) != Some("chat-worker")
                    {
                        continue;
                    }
                    if crate::management::receipt(home, job.id)?
                        .is_some_and(|r| !r["claimed_at"].is_null())
                    {
                        bail!("cannot park this goal while its chat owns a claimed grant; finish or yield that work first");
                    }
                }
                command.push("--park-goal".into());
                let existing = call(
                    home,
                    &Request::Status {
                        queue: Some(queue.clone()),
                    },
                )?;
                if let Some(job) = existing
                    .as_array()
                    .context("invalid job list")?
                    .iter()
                    .find(|job| job["key"] == key)
                {
                    if job["command"] != serde_json::to_value(&command)?
                        || job["cwd"] != text(&cwd)?
                    {
                        bail!("key already exists with different inputs; goal unchanged");
                    }
                    return print(
                        json!({"id":job["id"],"queue":job["queue"],"state":job["state"]}),
                    );
                }
                let pending = if supported {
                    crate::batch::pending(home, &queue, &thread)?
                } else {
                    None
                };
                if let Some((id, primary_key, primary_command)) = pending {
                    let owner = crate::batch::argument(&primary_command, "--goal-key")
                        .unwrap_or(&primary_key)
                        .to_owned();
                    let receipt = crate::management::receipt(home, id)?;
                    let accepted = receipt.as_ref().is_some_and(|r| !r["message_id"].is_null());
                    let delivered = accepted
                        && receipt
                            .as_ref()
                            .is_some_and(|r| r["state"] == "message_queued");
                    if accepted
                        && !delivered
                        && receipt.as_ref().is_some_and(|r| r["state"] != "deferred")
                    {
                        bail!("the existing invitation has uncertain delivery; inspect it before adding a parked goal");
                    }
                    if delivered {
                        goal_already_dispatched = true;
                    } else {
                        super::goal::park(home, &codex, &thread, &queue, &owner)?;
                    }
                    command.push("--batch-join-only".into());
                    command.extend(["--goal-key".into(), owner]);
                } else {
                    super::goal::park(home, &codex, &thread, &queue, &key)?;
                }
            }
            let mut result = submit(home, queue, key, cwd, command).context("submission failed; if --park-goal was used, inspect HOME/goals and explicitly unpark the unchanged goal")?;
            if goal_already_dispatched {
                result["goal"] = json!("already_dispatched_not_paused");
            }
            print(result)
        }
        Actions::ConfigureWake { opener } => {
            if !opener.is_absolute() {
                bail!("native URL opener must be an absolute executable path");
            }
            let opener = executable(&opener)?;
            fs::create_dir_all(home)?;
            let path = home.join("app-wake.json");
            let temporary = path.with_extension("json.tmp");
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temporary)?;
            serde_json::to_writer_pretty(&mut file, &json!({"opener":opener}))?;
            file.sync_all()?;
            fs::rename(temporary, &path)?;
            print(json!({"configured":path,"opener":opener}))
        }
        Actions::UnparkGoal { thread, codex } => {
            validate_thread(&thread)?;
            super::goal::restore(home, &executable(&codex)?, &thread, None)?;
            print(json!({"thread":thread,"goal":"restored"}))
        }
        Actions::Claim { id, thread } => reply(home, id, &thread, "claim", false),
        Actions::Finish { id, thread, failed } => reply(home, id, &thread, "finish", failed),
        Actions::Acknowledge { id, thread } => reply(home, id, &thread, "acknowledge", false),
        Actions::Notify { id, thread, codex } => {
            validate_thread(&thread)?;
            let jobs = call(home, &Request::Status { queue: None })?;
            if !jobs
                .as_array()
                .context("invalid jobs")?
                .iter()
                .any(|j| j["id"] == id)
            {
                bail!("unknown target job");
            }
            let codex = executable(&codex)?;
            let command = own_command(
                home,
                vec![
                    "notify-worker".into(),
                    id.to_string(),
                    "--thread".into(),
                    thread.clone(),
                    "--codex".into(),
                    text(&codex)?,
                ],
            )?;
            print(submit(
                home,
                observer_queue(id, &thread),
                "notify".into(),
                fs::canonicalize(home)?,
                command,
            )?)
        }
        Actions::ChatWorker {
            queue,
            key,
            thread,
            codex,
            message,
            park_goal,
            managed_commands,
            batch_requests,
            batch_join_only,
            goal_key,
        } => {
            if batch_join_only {
                bail!("a join-only request must never run as a standalone worker");
            }
            let job = running_job(home, &queue, &key)?;
            let id = job["id"].as_i64().context("missing job ID")?;
            let mut prompt=format!("Monohero: your turn in queue {queue}. Job {id}; worktree {}. The human owner authorized this existing chat to continue: {message}\nFirst confirm receipt using:\n{}\nIf claim fails, stop without doing protected work. Then complete the already-authorized protected work, preserving project approvals/checks/review/deploy/verification. Do not poll or enqueue recursively. After ALL protected work, execute:\n{}\nOn failure/blocker append --failed and explain. Do no protected work after finish. Acknowledging the grant alone is not completion.",job["cwd"].as_str().unwrap_or(""),control(home,"claim",id,&thread)?,control(home,"finish",id,&thread)?);
            if batch_requests {
                prompt.push_str("\nRead the claim response. When it includes a requests array, account for each accumulated request in order and use its recorded worktree before finish.");
            }
            if managed_commands {
                prompt.push_str(&format!("\nThis is a managed, revocable grant. Run EVERY protected shell/Git/test/push/review/merge/deploy command through `{} --home {} run {id} --thread {thread} -- COMMAND ARGS`. Use command-line tools for protected mutations so the lease lock covers the complete action. Wait for all protected children. Before asking the human for input, yield safely with `{} --home {} defer {id} --thread {thread} --reason REASON` and stop protected work. The native monitor may revoke blocked managed grants after protected children drain. Never continue protected work after revocation or use a stale invitation; explicitly resume as a new queue entry. Failed lease validation requires stopping, not bypassing the wrapper.",text(&std::env::current_exe()?)?,text(home)?,text(&std::env::current_exe()?)?,text(home)?));
            }
            receive(
                home,
                id,
                &thread,
                "lease",
                &codex,
                &prompt,
                ReceivePolicy {
                    initial_goal: park_goal
                        .then_some((queue.as_str(), goal_key.as_deref().unwrap_or(key.as_str()))),
                    managed: managed_commands,
                    batch_owner: batch_requests
                        .then_some((queue.as_str(), goal_key.as_deref().unwrap_or(key.as_str()))),
                },
            )
        }
        Actions::NotifyWorker { id, thread, codex } => {
            let job = running_job(home, &observer_queue(id, &thread), "notify")?;
            let notice = job["id"].as_i64().context("missing notification job ID")?;
            let result = call(home, &Request::Wait { id })?;
            let prompt=format!("Monohero completion notice for job {id}; notification {notice}. The human owner authorized this existing chat to resume. First confirm receipt:\n{}\nActual job result:\n{}\nRead the saved evidence and report the outcome. Continue only already-authorized follow-up work. This notice grants NO release slot: new protected work must rejoin its project queue. Do not retry failed deployment automatically.",control(home,"acknowledge",notice,&thread)?,serde_json::to_string_pretty(&result)?);
            receive(
                home,
                notice,
                &thread,
                "notice",
                &codex,
                &prompt,
                ReceivePolicy {
                    initial_goal: None,
                    managed: false,
                    batch_owner: None,
                },
            )
        }
    }
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn control(home: &Path, op: &str, id: i64, thread: &str) -> Result<String> {
    let args = [
        text(&std::env::current_exe()?)?,
        "--home".into(),
        text(home)?,
        op.into(),
        id.to_string(),
        "--thread".into(),
        thread.into(),
    ];
    Ok(args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" "))
}
struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct ReceivePolicy<'a> {
    initial_goal: Option<(&'a str, &'a str)>,
    managed: bool,
    batch_owner: Option<(&'a str, &'a str)>,
}
fn receive(
    home: &Path,
    id: i64,
    thread: &str,
    kind: &str,
    codex: &Path,
    prompt: &str,
    policy: ReceivePolicy<'_>,
) -> Result<()> {
    let ReceivePolicy {
        initial_goal,
        managed,
        batch_owner,
    } = policy;
    let batched = batch_owner.is_some();
    validate_thread(thread)?;
    let dir = private_dir(home)?;
    let socket = dir.join(format!("{id}.sock"));
    let path = dir.join(format!("{id}.json"));
    let listener = UnixListener::bind(&socket)
        .context("chat socket exists; inspect its interrupted owner before retrying")?;
    let _socket = SocketGuard(socket.clone());
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let mut r = Receipt {
        job_id: id,
        thread_id: thread.into(),
        kind: kind.into(),
        state: "waiting_for_thread".into(),
        message_id: None,
        claimed_at: None,
        updated_at: now(),
        finish_requested: None,
        error: None,
        app_wake: None,
        managed,
    };
    persist(&path, &mut r)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join(format!("{thread}.lock")))?;
    lock.lock_exclusive()?;
    // A surviving dispatcher retains ownership if its worker is interrupted.
    use std::os::fd::AsRawFd;
    unsafe {
        if libc::fcntl(lock.as_raw_fd(), libc::F_SETFD, 0) == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let admission = batch_owner
        .map(|(queue, _)| crate::batch::admission(home, queue, thread))
        .transpose()?;
    let parked_goal = if let Some((queue, key)) = batch_owner {
        if crate::goal::parked_for(home, thread, queue, key)? {
            Some((queue, key))
        } else {
            initial_goal
        }
    } else {
        initial_goal
    };
    r.state = "dispatching".into();
    persist(&path, &mut r)?;
    match dispatch(codex, thread, prompt, home, id) {
        Ok(message) => {
            r.message_id = Some(message);
            // Save accepted delivery before any external wake; never resend on wake failure.
            persist(&path, &mut r)?;
            if let Err(error) = wake_app(home, thread, id, &path, &mut r) {
                r.state = "failed".into();
                r.error = Some(format!("native app wake failed after message acceptance: {error:#}; inspect before retrying"));
                persist(&path, &mut r)?;
                return Err(error);
            }
            if let Some(owner) = parked_goal {
                if let Err(error) = super::goal::restore(home, codex, thread, Some(owner)) {
                    r.state = "failed".into();
                    r.error = Some(format!("{error:#}"));
                    persist(&path, &mut r)?;
                    return Err(error);
                }
            }
            r.state = "message_queued".into();
            persist(&path, &mut r)?;
        }
        Err(e) => {
            r.state = "failed".into();
            r.error = Some(format!("{e:#}"));
            persist(&path, &mut r)?;
            return Err(e);
        }
    }
    drop(admission);
    // CLI acceptance is not proof of resumption. Wait for the actual chat's acknowledgement.
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut requests = None;
        let result = (|| -> Result<bool> {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            let value: Value = serde_json::from_str(&line)?;
            if value["thread"] != thread {
                bail!("thread does not own this chat job");
            }
            match value["op"].as_str() {
                Some("claim") if kind == "lease" => {
                    let connection = crate::db(home)?;
                    if batched {
                        crate::batch::schema(&connection)?;
                        connection.execute_batch("BEGIN IMMEDIATE")?;
                    }
                    let claim = (|| -> Result<()> {
                        let status = call(home, &Request::Status { queue: None })?;
                        let current = status
                            .as_array()
                            .context("invalid job list")?
                            .iter()
                            .find(|job| job["id"] == id)
                            .context("unknown job")?;
                        if current["state"] != "running" || !current["pending_action"].is_null() {
                            bail!("grant no longer available; do not start protected work");
                        }
                        if batched {
                            let entries = crate::batch::seal(&connection, id)?;
                            if !entries.is_empty() {
                                requests = Some(entries);
                            }
                        }
                        r.claimed_at.get_or_insert_with(now);
                        r.state = "claimed".into();
                        persist(&path, &mut r)?;
                        Ok(())
                    })();
                    if batched {
                        if claim.is_ok() {
                            connection.execute_batch("COMMIT")?;
                        } else {
                            let _ = connection.execute_batch("ROLLBACK");
                        }
                    }
                    claim?;
                    Ok(false)
                }
                Some("finish") if kind == "lease" => {
                    if r.claimed_at.is_none() {
                        bail!("claim the grant in its destination chat before finishing");
                    }
                    let failed = value["failed"]
                        .as_bool()
                        .context("failed must be boolean")?;
                    let _resource = if managed {
                        Some(crate::fence::finish_guard(home, id)?)
                    } else {
                        None
                    };
                    if managed && !failed && crate::fence::has_failure(home, id) {
                        bail!("a protected command failed; inspect effects and finish --failed or explicitly yield");
                    }
                    r.finish_requested = Some(failed);
                    r.state = if failed { "failed" } else { "completed" }.into();
                    persist(&path, &mut r)?;
                    Ok(true)
                }
                Some(op @ ("defer" | "cancel")) if kind == "lease" => {
                    if r.claimed_at.is_some() && !managed && value["owner_yield"] != true {
                        bail!("legacy claimed work must be yielded by its owner");
                    }
                    r.state = if op == "defer" {
                        "deferred"
                    } else {
                        "cancelled"
                    }
                    .into();
                    persist(&path, &mut r)?;
                    Ok(true)
                }
                Some("acknowledge") if kind == "notice" => {
                    r.claimed_at = Some(now());
                    r.state = "completed".into();
                    persist(&path, &mut r)?;
                    Ok(true)
                }
                _ => bail!("operation does not match chat job kind"),
            }
        })();
        match result {
            Ok(done) => {
                let mut response = json!({"ok":true,"state":r.state});
                if let Some(entries) = requests {
                    response["requests"] = json!(entries);
                }
                writeln!(stream, "{}", response)?;
                if done {
                    if r.claimed_at.is_none()
                        && matches!(r.state.as_str(), "deferred" | "cancelled")
                    {
                        if let Some(message) = r.message_id.as_deref() {
                            if let Err(error) =
                                crate::monitor::remove_pending(codex, thread, message)
                            {
                                r.error=Some(format!("Grant revoked, but old queued message could not be withdrawn: {error:#}. A stale claim remains invalid."));
                                persist(&path, &mut r)?;
                            }
                        }
                    }
                    if r.finish_requested == Some(true) {
                        bail!("owner finished with failure; inspect its report");
                    }
                    return Ok(());
                }
            }
            Err(e) => {
                writeln!(stream, "{}", json!({"ok":false,"error":format!("{e:#}")}))?;
            }
        }
    }
    bail!("chat listener closed without completion")
}
fn dispatch(codex: &Path, thread: &str, prompt: &str, home: &Path, id: i64) -> Result<String> {
    let out = Command::new(codex)
        .args(["queue", "--thread", thread, "--message", prompt])
        .env("MONOHERO_HOME", home)
        .env("MONOHERO_JOB_ID", id.to_string())
        .env("MONOHERO_THREAD_ID", thread)
        .stdin(Stdio::null())
        .output()
        .context("unable to invoke codex queue")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    print!("{stdout}");
    eprint!("{stderr}");
    if !out.status.success() {
        bail!(
            "codex queue failed: {}; no successful delivery recorded",
            out.status
        );
    }
    for line in stdout.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() == 6
            && parts[0] == "Queued"
            && parts[1] == "message"
            && parts[3] == "for"
            && parts[4] == "thread"
            && parts[5].strip_suffix('.') == Some(thread)
            && validate_thread(parts[2]).is_ok()
        {
            return Ok(parts[2].into());
        }
    }
    bail!("Codex did not confirm the exact destination thread; delivery is uncertain, do not retry blindly")
}

fn wake_app(home: &Path, thread: &str, id: i64, path: &Path, receipt: &mut Receipt) -> Result<()> {
    let body = match fs::read(home.join("app-wake.json")) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        opener: PathBuf,
    }
    let config: Config = serde_json::from_slice(&body).context("invalid app-wake.json")?;
    if !config.opener.is_absolute() {
        bail!("configured URL opener must be absolute");
    }
    receipt.app_wake = Some("opening".into());
    persist(path, receipt)?;
    let status = Command::new(&config.opener)
        .arg(format!("codex://threads/{thread}"))
        .env("MONOHERO_HOME", home)
        .env("MONOHERO_JOB_ID", id.to_string())
        .env("MONOHERO_THREAD_ID", thread)
        .stdin(Stdio::null())
        .status()
        .context("unable to invoke native URL opener")?;
    if !status.success() {
        bail!("native URL opener failed: {status}");
    }
    receipt.app_wake = Some("requested".into());
    persist(path, receipt)
}

pub fn revoke(
    home: &Path,
    id: i64,
    thread: &str,
    operation: &str,
    owner_yield: bool,
) -> Result<()> {
    let mut stream = UnixStream::connect(home.join("chats").join(format!("{id}.sock")))
        .context("no live grant receiver")?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    writeln!(
        stream,
        "{}",
        json!({"thread":thread,"op":operation,"owner_yield":owner_yield})
    )?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let value: Value = serde_json::from_str(&line)?;
    if value["ok"] != true {
        bail!("{}", value["error"]);
    }
    Ok(())
}
