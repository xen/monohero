//! Revocation fences shared by all protected children of a managed chat grant.
use crate::{call, management, Request};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
fn directory(home: &Path) -> Result<PathBuf> {
    let path = home.join("leases");
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}
fn resource(home: &Path, id: i64) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory(home)?.join(format!("{id}.lock")))?)
}
pub fn inherited_work(home: &Path, id: i64) -> Result<File> {
    let file = resource(home, id)?;
    FileExt::lock_shared(&file)?;
    Ok(file)
}
pub fn stop(home: &Path, id: i64) -> Result<()> {
    let path = directory(home)?.join(format!("{id}.stopping"));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(b"Grant revocation requested; no new protected commands\n")?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn exclusive(home: &Path, id: i64) -> Result<File> {
    let file = resource(home, id)?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}
pub fn finish_guard(home: &Path, id: i64) -> Result<File> {
    let file = resource(home, id)?;
    FileExt::try_lock_exclusive(&file)
        .context("protected children are still running or grant revocation is draining; finish only after they exit")?;
    stop(home, id)?;
    Ok(file)
}
pub fn run(home: &Path, id: i64, thread: &str, command: &[String]) -> Result<()> {
    if command.is_empty() {
        bail!("a protected command is required");
    }
    let work = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("active-work.lock"))?;
    FileExt::lock_shared(&work)?;
    let resource = resource(home, id)?;
    FileExt::lock_shared(&resource)?;
    let stopped = directory(home)?.join(format!("{id}.stopping"));
    if stopped.exists() {
        bail!("grant is stopping/revoked; rejoin the queue before protected work");
    }
    let data = call(home, &Request::Status { queue: None })?;
    let job = data
        .as_array()
        .context("invalid status")?
        .iter()
        .find(|job| job["id"] == id)
        .context("unknown grant")?;
    let receipt = management::receipt(home, id)?.context("no chat grant receipt")?;
    if job["state"] != "running"
        || !job["pending_action"].is_null()
        || receipt["state"] != "claimed"
        || receipt["thread_id"] != thread
        || receipt["managed"] != true
        || stopped.exists()
    {
        bail!("only the current claimed managed grant can run protected commands");
    }
    // The lock survives a parent exit until every protected child closes it.
    unsafe {
        if libc::fcntl(work.as_raw_fd(), libc::F_SETFD, 0) == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        if libc::fcntl(resource.as_raw_fd(), libc::F_SETFD, 0) == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let status = match Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::inherit())
        .status()
    {
        Ok(status) => status,
        Err(error) => {
            record_failure(
                home,
                id,
                &format!("Protected command could not start: {error}"),
            )?;
            return Err(error).context("cannot start protected command");
        }
    };
    if !status.success() {
        record_failure(home, id, &format!("Protected command failed: {status}"))?;
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
fn record_failure(home: &Path, id: i64, reason: &str) -> Result<()> {
    let mut marker = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(directory(home)?.join(format!("{id}.command-failed")))?;
    writeln!(marker, "{reason}")?;
    marker.sync_all()?;
    Ok(())
}

pub fn has_failure(home: &Path, id: i64) -> bool {
    home.join("leases")
        .join(format!("{id}.command-failed"))
        .exists()
}
