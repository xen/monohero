//! Launch the separately built GPUI Kit application; the queue service remains independent.
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
pub fn launch(home: &Path, hidden: bool, ui: Option<PathBuf>) -> Result<()> {
    let executable = match ui {
        Some(path) => path,
        None => std::env::current_exe()?
            .parent()
            .context("CLI has no parent directory")?
            .join("monohero-ui"),
    };
    let mut command = Command::new(&executable);
    command.arg("--home").arg(home).stdin(Stdio::null());
    if hidden {
        command.arg("--hidden");
    }
    let child = command.spawn().with_context(|| {
        format!(
            "cannot launch GPUI Kit UI at {}; install monohero-ui beside the CLI or provide --ui",
            executable.display()
        )
    })?;
    println!(
        "Monohero UI launch requested (PID {}). Queue service remains independent.",
        child.id()
    );
    Ok(())
}
