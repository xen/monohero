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
fn failure_barrier_cancel_and_resolution() {
    let s = server();
    let h = s.home.path();
    let a = submit(h, "a", "bad", "exit 7");
    assert!(!wait(h, a).status.success());
    let b = submit(h, "a", "next", "echo next");
    assert_eq!(state(h, b), "queued");
    let c = submit(h, "a", "cancel", "echo never");
    json(h, &["cancel", &c.to_string()]);
    assert_eq!(state(h, c), "cancelled");
    assert!(!cli(h, &["cancel", &a.to_string()]).status.success());
    json(h, &["resolve", &a.to_string()]);
    assert!(wait(h, b).status.success());
}
#[test]
fn restart_retains_queue_and_dedup() {
    let mut s = server();
    let h = s.home.path();
    let a = submit(h, "a", "fail", "exit 1");
    wait(h, a);
    let b = submit(h, "a", "pending", "echo persisted");
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    s.child = start(h);
    assert_eq!(state(h, b), "queued");
    assert_eq!(b, submit(h, "a", "pending", "echo persisted"));
    json(h, &["resolve", &a.to_string()]);
    assert!(wait(h, b).status.success());
}
#[test]
fn surviving_child_prevents_restart_then_interruption_blocks_queue() {
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
    assert_eq!(state(h, b), "queued");
    json(h, &["resolve", &a.to_string()]);
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
