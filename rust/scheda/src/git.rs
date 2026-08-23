use std::path::Path;
use std::process::Command;

use crate::Error;

pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| Error::git(format!("failed to spawn git: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let joined = args.join(" ");
        return Err(Error::git(format!("git {joined} failed: {stderr}")));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| Error::git("git output was not UTF-8"))?;
    Ok(stdout.trim_end().to_owned())
}

pub(crate) fn is_work_tree(dir: &Path) -> bool {
    dir.join(".git").exists()
}

pub(crate) fn remotes(dir: &Path) -> Result<Vec<String>, Error> {
    let out = run(dir, &["remote"])?;
    Ok(out
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}
