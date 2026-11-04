//! Durable chat submissions joined to one FIFO grant before claim.
use crate::{jobs, name, signal, Shared};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

pub fn schema(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS chat_batches(job_id INTEGER PRIMARY KEY, queue TEXT NOT NULL, thread TEXT NOT NULL, sealed INTEGER NOT NULL DEFAULT 0); CREATE INDEX IF NOT EXISTS chat_batches_lookup ON chat_batches(queue,thread,sealed); CREATE TABLE IF NOT EXISTS chat_batch_requests(sequence INTEGER PRIMARY KEY AUTOINCREMENT, origin_job_id INTEGER NOT NULL, job_id INTEGER NOT NULL, queue TEXT NOT NULL, key TEXT NOT NULL, cwd TEXT NOT NULL, command TEXT NOT NULL, UNIQUE(queue,key)); CREATE INDEX IF NOT EXISTS chat_batch_requests_job ON chat_batch_requests(job_id,sequence); CREATE INDEX IF NOT EXISTS chat_batch_requests_origin ON chat_batch_requests(origin_job_id,sequence);")?;
    Ok(())
}
pub fn capable(home: &Path) -> Result<bool> {
    match crate::call(home, &crate::Request::Snapshot) {
        Ok(value) => Ok(value["capabilities"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == "chat_request_batches_v1"))),
        Err(error) if format!("{error:#}").contains("unknown variant") => Ok(false),
        Err(error) => Err(error),
    }
}
pub fn admission(home: &Path, queue: &str, thread: &str) -> Result<File> {
    let directory = home.join("chats");
    fs::create_dir_all(&directory)?;
    // Stable across separately built client and worker versions; a collision
    // only serializes unrelated submissions because SQLite matches exact IDs.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in queue
        .bytes()
        .chain(std::iter::once(0))
        .chain(thread.bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(format!("admission-{hash:016x}.lock")))?;
    file.lock_exclusive()?;
    Ok(file)
}
pub fn pending(
    home: &Path,
    queue: &str,
    thread: &str,
) -> Result<Option<(i64, String, Vec<String>)>> {
    let c = crate::db(home)?;
    let row:Option<(i64,String,String)>=c.query_row("SELECT j.id,j.key,j.command FROM chat_batches b JOIN jobs j ON j.id=b.job_id WHERE b.queue=?1 AND b.thread=?2 AND b.sealed=0 AND j.archived=0 AND (j.pending_action IS NULL OR j.pending_action='defer') AND j.state IN ('queued','running','deferred') ORDER BY j.position,j.id LIMIT 1",params![queue,thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    row.map(|(id, key, command)| Ok((id, key, serde_json::from_str(&command)?)))
        .transpose()
}
pub fn lookup(home: &Path, queue: &str, key: &str) -> Result<Option<(String, Vec<String>)>> {
    let c = crate::db(home)?;
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT cwd,command FROM chat_batch_requests WHERE queue=?1 AND key=?2",
            params![queue, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(cwd, command)| Ok((cwd, serde_json::from_str(&command)?)))
        .transpose()
}
pub fn argument<'a>(command: &'a [String], name: &str) -> Option<&'a str> {
    command
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}
fn chat_thread(command: &[String]) -> Option<&str> {
    (command.get(3).map(String::as_str) == Some("chat-worker"))
        .then(|| argument(command, "--thread"))
        .flatten()
}
pub fn submit(
    c: &Connection,
    s: &Shared,
    queue: &str,
    key: &str,
    cwd: &Path,
    command: &[String],
) -> Result<Option<Value>> {
    if !command.iter().any(|arg| arg == "--batch-requests") {
        return Ok(None);
    }
    let thread = chat_thread(command).context("batched submission requires a chat worker")?;
    crate::chat::validate_thread(thread)?;
    if command.get(1).map(String::as_str) != Some("--home")
        || command
            .get(2)
            .map(Path::new)
            .map(fs::canonicalize)
            .transpose()?
            != Some(fs::canonicalize(&s.home)?)
        || argument(command, "--queue") != Some(queue)
        || argument(command, "--key") != Some(key)
        || argument(command, "--message").is_none()
        || argument(command, "--codex").is_none()
    {
        bail!("batched chat worker identity or inputs do not match the submission");
    }
    name(queue)?;
    name(key)?;
    let cwd = cwd.to_str().context("non UTF-8 cwd")?;
    let encoded = serde_json::to_string(command)?;
    c.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<Value> {
        if let Some((origin, id, old_cwd, old_command)) = c
            .query_row(
                "SELECT origin_job_id,job_id,cwd,command FROM chat_batch_requests WHERE queue=?1 AND key=?2",
                params![queue, key],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        {
            if old_cwd != cwd || old_command != encoded {
                bail!("key already exists with different inputs; choose a new key");
            }
            let state: String = c.query_row("SELECT state FROM jobs WHERE id=?1", [origin], |r| r.get(0))?;
            return Ok(json!({"id":origin,"current_job":id,"queue":queue,"state":state,"coalesced":true}));
        }
        if c.query_row(
            "SELECT COUNT(*) FROM jobs WHERE queue=?1 AND key=?2",
            params![queue, key],
            |r| r.get::<_, i64>(0),
        )? != 0
        {
            bail!("key already belongs to a prior submission");
        }
        let existing:Option<i64> = c.query_row("SELECT b.job_id FROM chat_batches b JOIN jobs j ON j.id=b.job_id WHERE b.queue=?1 AND b.thread=?2 AND b.sealed=0 AND j.archived=0 AND (j.pending_action IS NULL OR j.pending_action='defer') AND j.state IN ('queued','running','deferred') ORDER BY j.position,j.id LIMIT 1",params![queue,thread],|r|r.get(0)).optional()?;
        let id = if let Some(id) = existing {
            let first: String = c.query_row(
                "SELECT command FROM chat_batch_requests WHERE job_id=?1 ORDER BY sequence LIMIT 1",
                [id],
                |r| r.get(0),
            )?;
            let first: Vec<String> = serde_json::from_str(&first)?;
            if argument(&first, "--codex") != argument(command, "--codex") {
                bail!("chat request uses a different Codex executable; inspect the existing grant");
            }
            if command.iter().any(|x| x == "--park-goal") {
                let primary_key: String =
                    c.query_row("SELECT key FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
                let owner = argument(&first, "--goal-key").unwrap_or(&primary_key);
                if argument(command, "--goal-key").unwrap_or(key) != owner {
                    bail!("parked goal owner changed before this request could join");
                }
            }
            id
        } else {
            if command.iter().any(|x| x == "--batch-join-only") {
                bail!("the prior grant was claimed or changed before this request could join; retry explicitly");
            }
            // A legacy worker cannot deliver added requests. Preserve it and fail explicitly.
            for j in jobs(c)?.iter().filter(|j| {
                j.queue == queue
                    && !j.archived
                    && matches!(j.state.as_str(), "queued" | "running" | "deferred")
                    && crate::management::thread_id(j).as_deref() == Some(thread)
                    && chat_thread(&j.command).is_some()
            }) {
                let belongs_to_batch: i64 = c.query_row(
                    "SELECT COUNT(*) FROM chat_batches WHERE job_id=?1",
                    [j.id],
                    |r| r.get(0),
                )?;
                if belongs_to_batch > 0 {
                    continue;
                }
                let claimed = crate::management::receipt(&s.home, j.id)?
                    .is_some_and(|receipt| !receipt["claimed_at"].is_null());
                if !claimed {
                    bail!("existing legacy chat grant cannot accumulate requests; wait for its claim or completion");
                }
            }
            c.execute(
                "INSERT INTO queues(name) VALUES(?1) ON CONFLICT(name) DO UPDATE SET archived=0",
                [queue],
            )?;
            c.execute("INSERT INTO jobs(queue,key,cwd,command,state,callback,callback_state,position) VALUES(?1,?2,?3,?4,'queued',NULL,'none',(SELECT COALESCE(MAX(position),0)+1 FROM jobs WHERE queue=?1))",params![queue,key,cwd,encoded])?;
            let id = c.last_insert_rowid();
            c.execute(
                "INSERT INTO chat_batches(job_id,queue,thread) VALUES(?1,?2,?3)",
                params![id, queue, thread],
            )?;
            id
        };
        c.execute(
            "INSERT INTO chat_batch_requests(origin_job_id,job_id,queue,key,cwd,command) VALUES(?1,?1,?2,?3,?4,?5)",
            params![id, queue, key, cwd, encoded],
        )?;
        let state: String =
            c.query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
        Ok(json!({"id":id,"queue":queue,"state":state,"coalesced":existing.is_some()}))
    })();
    match result {
        Ok(value) => {
            c.execute_batch("COMMIT")?;
            signal(s);
            Ok(Some(value))
        }
        Err(error) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}
/// Called while the claim transaction has exclusive write admission.
pub fn seal(c: &Connection, id: i64) -> Result<Vec<Value>> {
    let exists = c.execute("UPDATE chat_batches SET sealed=1 WHERE job_id=?1", [id])?;
    if exists == 0 {
        return Ok(Vec::new());
    }
    let mut query = c.prepare(
        "SELECT key,cwd,command FROM chat_batch_requests WHERE job_id=?1 ORDER BY sequence",
    )?;
    let entries = query
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries
        .into_iter()
        .map(|(key, cwd, encoded)| {
            let command: Vec<String> = serde_json::from_str(&encoded)?;
            let message =
                argument(&command, "--message").context("batched request lacks message")?;
            Ok(json!({"key":key,"cwd":cwd,"message":message}))
        })
        .collect()
}
pub fn requeue(c: &Connection, old: i64, new: i64) -> Result<()> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT queue,thread FROM chat_batches WHERE job_id=?1",
            [old],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((queue, thread)) = row {
        c.execute(
            "INSERT INTO chat_batches(job_id,queue,thread) VALUES(?1,?2,?3)",
            params![new, queue, thread],
        )?;
        c.execute(
            "UPDATE chat_batch_requests SET job_id=?1 WHERE job_id=?2",
            params![new, old],
        )?;
    }
    Ok(())
}

/// Read request history without changing a job or retrying delivery.
pub fn requests(home: &Path, id: i64) -> Result<Value> {
    if !home.join("jobs.sqlite3").is_file() {
        bail!("Monohero state database does not exist");
    }
    let c = crate::db(home)?;
    let table: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chat_batch_requests'",
        [],
        |r| r.get(0),
    )?;
    if table == 0 {
        bail!("this service has no accumulated-request records");
    }
    let records = jobs(&c)?;
    let mut cursor = id;
    let mut ancestors = std::collections::HashSet::new();
    loop {
        if !ancestors.insert(cursor) {
            bail!("invalid request ancestry");
        }
        let job = records
            .iter()
            .find(|job| job.id == cursor)
            .context("unknown job or missing request ancestor")?;
        match job.parent_id {
            Some(parent) if parent < cursor => cursor = parent,
            Some(_) => bail!("invalid request ancestry"),
            None => break,
        }
    }
    let mut query=c.prepare("SELECT sequence,origin_job_id,job_id,key,cwd,command FROM chat_batch_requests WHERE origin_job_id=?1 ORDER BY sequence")?;
    let mut entries = Vec::new();
    for ancestor in ancestors {
        let rows = query.query_map([ancestor], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (sequence, origin, current, key, cwd, command) = row?;
            let command: Vec<String> = serde_json::from_str(&command)?;
            entries.push((sequence,json!({"sequence":sequence,"origin_job":origin,"current_job":current,"key":key,"cwd":cwd,"message":argument(&command,"--message").context("batched request lacks message")?})));
        }
    }
    entries.sort_by_key(|(sequence, _)| *sequence);
    Ok(
        json!({"job_id":id,"requests":entries.into_iter().map(|(_,value)|value).collect::<Vec<_>>()}),
    )
}
