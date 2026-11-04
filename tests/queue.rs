use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
struct Server {
    child: Child,
    home: TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn cli(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_monohero"))
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}
fn json(home: &Path, args: &[&str]) -> Value {
    let r = cli(home, args);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    serde_json::from_slice(&r.stdout).unwrap()
}
fn start(home: &Path) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_monohero"))
        .arg("--home")
        .arg(home)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cli(home, &["status"]).status.success() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    child
}
fn server() -> Server {
    let home = tempfile::tempdir().unwrap();
    let child = start(home.path());
    Server { child, home }
}
fn submit(home: &Path, queue: &str, key: &str, script: &str) -> i64 {
    json(
        home,
        &[
            "submit",
            "--queue",
            queue,
            "--key",
            key,
            "--cwd",
            home.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            script,
        ],
    )["id"]
        .as_i64()
        .unwrap()
}
fn wait(home: &Path, id: i64) -> std::process::Output {
    cli(home, &["wait", &id.to_string()])
}
fn state(home: &Path, id: i64) -> String {
    json(home, &["status"])
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .unwrap()["state"]
        .as_str()
        .unwrap()
        .into()
}
fn await_state(home: &Path, id: i64, wanted: &str) {
    let until = Instant::now() + Duration::from_secs(10);
    while state(home, id) != wanted {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn fifo_dedup_and_independent_queues() {
    let s = server();
    let h = s.home.path();
    assert!(Command::new("mkfifo")
        .arg(h.join("gate"))
        .status()
        .unwrap()
        .success());
    let script = "read x < gate; echo one >> order";
    let a = submit(h, "a", "one", script);
    assert_eq!(a, submit(h, "a", "one", script));
    let b = submit(h, "a", "two", "echo two >> order");
    let c = submit(h, "b", "other", "echo independent");
    assert!(wait(h, c).status.success());
    assert_ne!(state(h, a), "succeeded");
    fs::write(h.join("gate"), "go\n").unwrap();
    assert!(wait(h, b).status.success());
    assert_eq!(fs::read_to_string(h.join("order")).unwrap(), "one\ntwo\n");
    assert!(!cli(
        h,
        &["submit", "--queue", "a", "--key", "one", "--", "/bin/true"]
    )
    .status
    .success());
}
#[test]
fn failed_job_does_not_block_following_jobs() {
    let s = server();
    let h = s.home.path();
    let a = submit(h, "a", "bad", "exit 7");
    assert!(!wait(h, a).status.success());
    let b = submit(h, "a", "next", "echo next");
    assert!(wait(h, b).status.success());
    json(h, &["pause-queue", "a"]);
    let c = submit(h, "a", "cancel", "echo never");
    json(h, &["cancel", &c.to_string()]);
    assert_eq!(state(h, c), "cancelled");
    assert!(!cli(h, &["cancel", &a.to_string()]).status.success());
    json(h, &["resolve", &a.to_string()]);
    assert!(wait(h, b).status.success());
}
#[test]
fn failed_command_waits_for_surviving_child_before_next_job() {
    let s = server();
    let h = s.home.path();
    assert!(Command::new("mkfifo")
        .arg(h.join("gate"))
        .status()
        .unwrap()
        .success());
    let a = submit(h, "a", "child", "(touch started; read x < gate) & exit 1");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !h.join("started").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let b = submit(h, "a", "next", "true");
    assert_eq!(state(h, b), "queued");
    fs::write(h.join("gate"), "go\n").unwrap();
    assert!(!wait(h, a).status.success());
    assert!(wait(h, b).status.success());
}
#[test]
fn restart_retains_queue_and_dedup() {
    let mut s = server();
    let h = s.home.path();
    let a = submit(h, "a", "fail", "exit 1");
    wait(h, a);
    json(h, &["pause-queue", "a"]);
    let b = submit(h, "a", "pending", "echo persisted");
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    s.child = start(h);
    assert_eq!(b, submit(h, "a", "pending", "echo persisted"));
    json(h, &["resume-queue", "a"]);
    assert!(wait(h, b).status.success());
}
#[test]
fn surviving_child_prevents_restart_then_queue_continues() {
    let mut s = server();
    let h = s.home.path();
    assert!(Command::new("mkfifo")
        .arg(h.join("gate"))
        .status()
        .unwrap()
        .success());
    let a = submit(h, "a", "running", "touch started; read x < gate; echo done");
    await_state(h, a, "running");
    // Ensure child has actually started and inherited ownership before killing daemon.
    let until = Instant::now() + Duration::from_secs(5);
    while !h.join("started").exists() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(100));
    let b = submit(h, "a", "pending", "echo next");
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    let refused = cli(h, &["serve"]);
    assert!(!refused.status.success());
    fs::write(h.join("gate"), "go\n").unwrap();
    thread::sleep(Duration::from_millis(100));
    s.child = start(h);
    assert_eq!(state(h, a), "interrupted");
    assert!(wait(h, b).status.success());
}
#[test]
fn missing_executable_is_failure_and_callback_runs() {
    let s = server();
    let h = s.home.path();
    let callback = h.join("callback.sh");
    fs::write(
        &callback,
        "#!/bin/sh\nprintf '%s %s' \"$1\" \"$2\" > callback-result\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&callback, fs::Permissions::from_mode(0o700)).unwrap();
    let j = json(
        h,
        &[
            "submit",
            "--queue",
            "a",
            "--key",
            "missing",
            "--on-complete",
            callback.to_str().unwrap(),
            "--cwd",
            h.to_str().unwrap(),
            "--",
            "/does/not/exist",
        ],
    );
    let id = j["id"].as_i64().unwrap();
    assert!(!wait(h, id).status.success());
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let all = json(h, &["status"]);
        if all[0]["callback_state"] == "succeeded" {
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(h.join("callback-result")).unwrap(),
        format!("{id} failed")
    );
}

#[test]
fn worktrees_share_queue_but_keep_their_own_working_directories() {
    let s = server();
    let h = s.home.path();
    let a = h.join("worktree-a");
    let b = h.join("worktree-b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let script = "#!/bin/sh\npwd > result\n";
    use std::os::unix::fs::PermissionsExt;
    for dir in [&a, &b] {
        fs::write(dir.join("run.sh"), script).unwrap();
        fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let submit_from = |dir: &Path, key: &str| {
        let r = Command::new(env!("CARGO_BIN_EXE_monohero"))
            .arg("--home")
            .arg(h)
            .args([
                "submit", "--queue", "shared", "--key", key, "--", "./run.sh",
            ])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        serde_json::from_slice::<Value>(&r.stdout).unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let first = submit_from(&a, "a");
    let second = submit_from(&b, "b");
    assert!(first < second);
    assert!(wait(h, second).status.success());
    for dir in [&a, &b] {
        assert_eq!(
            fs::canonicalize(fs::read_to_string(dir.join("result")).unwrap().trim()).unwrap(),
            fs::canonicalize(dir).unwrap()
        );
    }
}

#[test]
fn readable_queue_shows_failures_fifo_and_project_isolation() {
    let server = server();
    let home = server.home.path();
    let failed = submit(home, "project-a", "failed-release", "exit 9");
    assert!(!wait(home, failed).status.success());
    json(home, &["pause-queue", "project-a"]);
    let first = submit(home, "project-a", "first-waiter", "exit 0");
    let second = submit(home, "project-a", "second-waiter", "exit 0");
    let other = submit(home, "project-b", "other-project", "exit 0");
    assert!(wait(home, other).status.success());
    let before = json(home, &["status"]);
    let output = cli(home, &["queue", "--queue", "project-a"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&format!("#{failed}")), "{text}");
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("waiting 1"));
    assert!(text.contains("waiting 2"));
    assert!(text.find("first-waiter").unwrap() < text.find("second-waiter").unwrap());
    assert!(!text.contains("project-b"));
    assert!(!text.contains("other-project"));
    let selected = json(home, &["queue", "--queue", "project-a", "--json"]);
    let ids: Vec<i64> = selected
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![failed, first, second]);
    assert_eq!(
        json(home, &["status"]),
        before,
        "viewing must not mutate jobs"
    );
    let empty = cli(home, &["queue", "--queue", "project-b"]);
    assert_eq!(
        String::from_utf8(empty.stdout).unwrap(),
        "No jobs to display.\n"
    );
    let history = json(home, &["queue", "--queue", "project-b", "--all", "--json"]);
    assert_eq!(history[0]["id"], other);
}

#[test]
fn manual_order_and_pause_survive_restart_and_reject_stale_lists() {
    let mut s = server();
    let h = s.home.path();
    json(
        h,
        &["policy", "--queue", "ordered", "--blocked", "attention"],
    );
    json(h, &["pause-queue", "ordered"]);
    let a = submit(h, "ordered", "a", "echo a >> order");
    let b = submit(h, "ordered", "b", "echo b >> order");
    let c = submit(h, "ordered", "c", "echo c >> order");
    json(
        h,
        &[
            "reorder",
            "--queue",
            "ordered",
            "--ids",
            &format!("{c},{a},{b}"),
        ],
    );
    let d = submit(h, "ordered", "d", "echo d >> order");
    assert!(!cli(
        h,
        &[
            "reorder",
            "--queue",
            "ordered",
            "--ids",
            &format!("{c},{a},{b}")
        ]
    )
    .status
    .success());
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    s.child = start(h);
    let snapshot = json(h, &["inspect"]);
    assert_eq!(snapshot["queues"][0]["paused"], true);
    let order: Vec<i64> = snapshot["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![c, a, b, d]);
    json(h, &["resume-queue", "ordered"]);
    assert!(wait(h, d).status.success());
    assert_eq!(fs::read_to_string(h.join("order")).unwrap(), "c\na\nb\nd\n");
}

#[test]
fn deferred_resume_uses_new_identity_and_clear_preserves_deduplication() {
    let s = server();
    let h = s.home.path();
    json(h, &["policy", "--queue", "p", "--blocked", "attention"]);
    json(h, &["pause-queue", "p"]);
    let a = submit(h, "p", "original", "echo executed > effect");
    json(h, &["defer", &a.to_string(), "--reason", "Needs input"]);
    let resumed = json(h, &["resume", &a.to_string()])["new_job"]
        .as_i64()
        .unwrap();
    assert_ne!(a, resumed);
    assert_eq!(submit(h, "p", "original", "echo executed > effect"), a);
    let snapshot = json(h, &["inspect"]);
    assert_eq!(snapshot["jobs"][0]["parent_id"], a);
    assert!(snapshot["jobs"][0].get("command").is_none());
    json(h, &["clear", "--queue", "p", "--delete"]);
    assert!(json(h, &["inspect"])["queues"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(submit(h, "p", "original", "echo executed > effect"), a);
    assert!(!h.join("effect").exists());
    assert!(json(h, &["status"])
        .as_array()
        .unwrap()
        .iter()
        .any(|job| job["id"] == resumed && job["state"] == "cancelled"));
}

#[test]
fn clear_rejects_active_work_and_uninspected_failure_barriers() {
    let s = server();
    let h = s.home.path();
    let failed = submit(h, "p", "failure", "exit 8");
    assert!(!wait(h, failed).status.success());
    assert!(!cli(h, &["clear", "--queue", "p"]).status.success());
    json(h, &["clear", "--queue", "p", "--acknowledge-failures"]);
    assert_eq!(json(h, &["status"])[0]["state"], "resolved");
    let running = submit(
        h,
        "p",
        "busy",
        "touch started; while [ ! -f release ]; do sleep 0.01; done",
    );
    let end = Instant::now() + Duration::from_secs(10);
    while !h.join("started").exists() {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!cli(
        h,
        &[
            "clear",
            "--queue",
            "p",
            "--delete",
            "--acknowledge-failures"
        ]
    )
    .status
    .success());
    fs::write(h.join("release"), "").unwrap();
    assert!(wait(h, running).status.success());
}
