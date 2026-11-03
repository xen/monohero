//! Human-readable snapshots over the existing, read-only status protocol.
use crate::{call, Job, Request};
use anyhow::Result;
use std::{collections::BTreeMap, fmt::Write, path::Path};

fn visible(job: &Job) -> bool {
    matches!(
        job.state.as_str(),
        "queued" | "running" | "failed" | "interrupted"
    ) || matches!(job.callback_state.as_str(), "running" | "failed")
}

pub fn show(home: &Path, queue: Option<String>, all: bool, json: bool) -> Result<()> {
    let data = call(home, &Request::Status { queue })?;
    let mut jobs: Vec<Job> = serde_json::from_value(data)?;
    jobs.sort_by_key(|job| job.id);
    jobs.retain(|job| all || visible(job));
    if json {
        println!("{}", serde_json::to_string_pretty(&jobs)?);
    } else {
        print!("{}", render(&jobs));
    }
    Ok(())
}

fn render(jobs: &[Job]) -> String {
    let mut groups = BTreeMap::<&str, Vec<&Job>>::new();
    for job in jobs {
        groups.entry(&job.queue).or_default().push(job);
    }
    if groups.is_empty() {
        return "No jobs to display.\n".into();
    }
    let mut output = String::new();
    for (queue, jobs) in groups {
        let blockers: Vec<String> = jobs
            .iter()
            .filter(|job| matches!(job.state.as_str(), "failed" | "interrupted"))
            .map(|job| format!("#{}", job.id))
            .collect();
        writeln!(output, "Queue: {queue}").unwrap();
        if !blockers.is_empty() {
            writeln!(
                output,
                "  BLOCKED by {} — inspect before resolve",
                blockers.join(", ")
            )
            .unwrap();
        }
        let mut position = 0;
        for job in jobs {
            let label = if job.state == "queued" {
                position += 1;
                format!("waiting {position}")
            } else {
                job.state.clone()
            };
            writeln!(output, "  #{:<5} {:<14} {}", job.id, label, job.key).unwrap();
            writeln!(output, "         cwd: {}", job.cwd.display()).unwrap();
            if let Some(worker) = job
                .command
                .iter()
                .position(|arg| arg == "chat-worker" || arg == "notify-worker")
            {
                if let Some(args) = job.command[worker..]
                    .windows(2)
                    .find(|args| args[0] == "--thread")
                {
                    writeln!(output, "         chat: {}", args[1]).unwrap();
                }
            }
            if matches!(job.callback_state.as_str(), "running" | "failed") {
                writeln!(output, "         callback: {}", job.callback_state).unwrap();
            }
        }
        output.push('\n');
    }
    output
}
