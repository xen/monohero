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
const THREAD: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";
fn stub(home: &Path, mode: &str) -> std::path::PathBuf {
    let path = home.join("codex-stub");
    let script = format!(
        r##"#!/usr/bin/env python3
import sys,os,json,pathlib,subprocess
home=pathlib.Path(os.environ['MONOHERO_HOME']);job=os.environ['MONOHERO_JOB_ID'];mode={mode:?}
if len(sys.argv)>1 and sys.argv[1]=='__agent__':
 thread=sys.argv[2];prompt=sys.argv[3]
 if mode=='unclaimed':
  (home/('ready-'+job)).touch();sys.exit(0)
 op='claim' if 'your turn in queue' in prompt else 'acknowledge'
 subprocess.run([{binary:?},'--home',str(home),op,job,'--thread',thread],check=True,stdout=subprocess.DEVNULL)
 (home/('ready-'+job)).touch()
 if op=='claim' and mode=='auto':
  subprocess.run([{binary:?},'--home',str(home),'finish',job,'--thread',thread],check=True,stdout=subprocess.DEVNULL)
 sys.exit(0)
assert sys.argv[1:3]==['queue','--thread'],sys.argv
assert sys.argv[4]=='--message',sys.argv
thread=sys.argv[3];prompt=sys.argv[5]
(home/('trace-'+job+'.json')).write_text(json.dumps({{'argv':sys.argv[1:],'prompt':prompt}}))
if mode=='fail':sys.exit(7)
if mode=='wrong':thread='33333333-3333-4333-8333-333333333333'
if mode=='malformed':
 print('accepted maybe',flush=True);sys.exit(0)
print('Queued message 44444444-4444-4444-8444-444444444444 for thread '+thread+'.',flush=True)
if mode not in ['wrong','queued-only']:
 subprocess.Popen([sys.executable,__file__,'__agent__',thread,prompt],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,close_fds=True)
"##,
        binary = env!("CARGO_BIN_EXE_monohero")
    );
    fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn enqueue(h: &Path, codex: &Path, queue: &str, key: &str, thread: &str) -> i64 {
    json(
        h,
        &[
            "enqueue",
            "--queue",
            queue,
            "--key",
            key,
            "--thread",
            thread,
            "--codex",
            codex.to_str().unwrap(),
            "--cwd",
            h.to_str().unwrap(),
        ],
    )["id"]
        .as_i64()
        .unwrap()
}
fn await_file(p: &Path) {
    let end = Instant::now() + Duration::from_secs(20);
    while !p.exists() {
        assert!(Instant::now() < end, "missing {}", p.display());
        thread::sleep(Duration::from_millis(20));
    }
}
fn receipt(h: &Path, id: i64) -> Value {
    serde_json::from_str(&fs::read_to_string(h.join("chats").join(format!("{id}.json"))).unwrap())
        .unwrap()
}
fn finish(h: &Path, id: i64) {
    json(h, &["finish", &id.to_string(), "--thread", THREAD]);
}
#[test]
fn chat_grant_is_fifo_and_requires_matching_claim_and_finish() {
    let s = server();
    let h = s.home.path();
    let codex = stub(h, "manual");
    let a = enqueue(h, &codex, "p", "a", THREAD);
    assert_eq!(a, enqueue(h, &codex, "p", "a", THREAD));
    let b = submit(h, "p", "b", "echo next > next");
    await_file(&h.join(format!("ready-{a}")));
    assert_eq!(state(h, b), "queued");
    assert_eq!(receipt(h, a)["state"], "claimed");
    assert!(!cli(h, &["finish", &a.to_string(), "--thread", OTHER])
        .status
        .success());
    finish(h, a);
    assert!(wait(h, b).status.success());
    assert_eq!(state(h, a), "succeeded");
    let trace: Value =
        serde_json::from_str(&fs::read_to_string(h.join(format!("trace-{a}.json"))).unwrap())
            .unwrap();
    assert_eq!(trace["argv"][2], THREAD);
}
#[test]
fn transport_acceptance_alone_is_not_a_claim() {
    let s = server();
    let h = s.home.path();
    let codex = stub(h, "unclaimed");
    let a = enqueue(h, &codex, "p", "a", THREAD);
    await_file(&h.join(format!("ready-{a}")));
    assert_eq!(receipt(h, a)["state"], "message_queued");
    assert!(!cli(h, &["finish", &a.to_string(), "--thread", THREAD])
        .status
        .success());
    json(h, &["claim", &a.to_string(), "--thread", THREAD]);
    finish(h, a);
    assert!(wait(h, a).status.success());
}
#[test]
fn wrong_thread_uncertain_delivery_and_connector_errors_block_queue() {
    for mode in ["wrong", "malformed", "fail"] {
        let s = server();
        let h = s.home.path();
        let codex = stub(h, mode);
        let a = enqueue(h, &codex, "p", "a", THREAD);
        assert!(!wait(h, a).status.success());
        let b = submit(h, "p", "b", "true");
        assert_eq!(state(h, b), "queued");
        json(h, &["resolve", &a.to_string()]);
        assert!(wait(h, b).status.success());
    }
}
#[test]
fn completed_command_wakes_owner_without_occupying_its_queue() {
    let s = server();
    let h = s.home.path();
    assert!(Command::new("mkfifo")
        .arg(h.join("gate"))
        .status()
        .unwrap()
        .success());
    let codex = stub(h, "auto");
    let a = submit(h, "p", "work", "read x < gate");
    await_state(h, a, "running");
    let target = a.to_string();
    let args = [
        "notify",
        &target,
        "--thread",
        THREAD,
        "--codex",
        codex.to_str().unwrap(),
    ];
    let notice = json(h, &args)["id"].as_i64().unwrap();
    assert_eq!(notice, json(h, &args)["id"].as_i64().unwrap());
    assert!(!h.join(format!("trace-{notice}.json")).exists());
    fs::write(h.join("gate"), "go\n").unwrap();
    assert!(wait(h, notice).status.success());
    let trace: Value =
        serde_json::from_str(&fs::read_to_string(h.join(format!("trace-{notice}.json"))).unwrap())
            .unwrap();
    assert!(trace["prompt"].as_str().unwrap().contains("succeeded"));
    assert!(trace["prompt"]
        .as_str()
        .unwrap()
        .contains("NO release slot"));
    assert_eq!(receipt(h, notice)["state"], "completed");
}
#[test]
fn failed_command_still_notifies_and_preserves_original_barrier() {
    let s = server();
    let h = s.home.path();
    let codex = stub(h, "auto");
    let a = submit(h, "p", "bad", "exit 1");
    wait(h, a);
    let notice = json(
        h,
        &[
            "notify",
            &a.to_string(),
            "--thread",
            THREAD,
            "--codex",
            codex.to_str().unwrap(),
        ],
    )["id"]
        .as_i64()
        .unwrap();
    assert!(wait(h, notice).status.success());
    let b = submit(h, "p", "next", "true");
    assert_eq!(state(h, b), "queued");
}
#[test]
fn one_thread_does_not_receive_two_simultaneous_grants() {
    let s = server();
    let h = s.home.path();
    let codex = stub(h, "manual");
    let a = enqueue(h, &codex, "a", "a", THREAD);
    await_file(&h.join(format!("ready-{a}")));
    let b = enqueue(h, &codex, "b", "b", THREAD);
    await_state(h, b, "running");
    assert!(!h.join(format!("ready-{b}")).exists());
    finish(h, a);
    await_file(&h.join(format!("ready-{b}")));
    finish(h, b);
    assert!(wait(h, b).status.success());
}
#[test]
fn invalid_thread_is_rejected_before_submission() {
    let s = server();
    let h = s.home.path();
    let codex = stub(h, "auto");
    assert!(!cli(
        h,
        &[
            "enqueue",
            "--queue",
            "p",
            "--key",
            "a",
            "--thread",
            "name",
            "--codex",
            codex.to_str().unwrap()
        ]
    )
    .status
    .success());
    assert_eq!(json(h, &["status"]).as_array().unwrap().len(), 0);
}
