#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::too_many_lines)]

pub mod config;

use anyhow::{format_err, Error};
use log::{debug, error};
use stack_string::{format_sstr, StackString};
use std::{
    path::Path,
    process::{ExitStatus, Stdio},
};
use stdout_channel::StdoutChannel;
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    task::{spawn, JoinHandle},
};

use config::Config;

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn get_first_line_of_file(fpath: &Path) -> Result<String, Error> {
    let mut buf = String::new();
    if fpath.exists() {
        if let Ok(f) = fs::File::open(fpath).await {
            let mut buf_read = BufReader::new(f);
            buf_read.read_line(&mut buf).await?;
        }
    }
    Ok(buf)
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn output_to_stdout(
    mut reader: BufReader<impl AsyncRead + Unpin>,
    eol: u8,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    let mut buf = Vec::new();
    while let Ok(bytes) = reader.read_until(eol, &mut buf).await {
        if bytes > 0 {
            stdout.send(format_sstr!(
                "{}",
                String::from_utf8_lossy(&buf).trim_end_matches('\n')
            ));
        } else {
            break;
        }
        buf.clear();
    }
    Ok(())
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn output_to_debug(
    mut reader: BufReader<impl AsyncRead + Unpin>,
    eol: u8,
) -> Result<(), Error> {
    let mut buf = Vec::new();
    while let Ok(bytes) = reader.read_until(eol, &mut buf).await {
        if bytes > 0 {
            debug!("{}", String::from_utf8_lossy(&buf));
        } else {
            break;
        }
        buf.clear();
    }
    Ok(())
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn output_to_error(
    mut reader: BufReader<impl AsyncRead + Unpin>,
    eol: u8,
) -> Result<(), Error> {
    let mut buf = Vec::new();
    while let Ok(bytes) = reader.read_until(eol, &mut buf).await {
        if bytes > 0 {
            error!("{}", String::from_utf8_lossy(&buf));
        } else {
            break;
        }
        buf.clear();
    }
    Ok(())
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn process_child(
    mut p: Child,
    stdout_channel: &StdoutChannel<StackString>,
) -> Result<ExitStatus, Error> {
    let stdout = p.stdout.take().ok_or_else(|| format_err!("No Stdout"))?;
    let stderr = p.stderr.take().ok_or_else(|| format_err!("No Stderr"))?;
    let reader = BufReader::new(stdout);
    let stdout_channel = stdout_channel.clone();
    let stdout_task: JoinHandle<Result<(), Error>> =
        spawn(async move { output_to_stdout(reader, b'\n', &stdout_channel).await });
    let reader = BufReader::new(stderr);
    let stderr_task: JoinHandle<Result<(), Error>> =
        spawn(async move { output_to_error(reader, b'\n').await });
    let status = p.wait().await?;
    stdout_task.await??;
    stderr_task.await??;
    Ok(status)
}

async fn process_git_directory_branch(repo_directory: &Path, branch: &str) -> Result<(), Error> {
    Command::new("git")
        .current_dir(repo_directory)
        .args(["checkout", branch])
        .status()
        .await?;
    Command::new("git")
        .current_dir(repo_directory)
        .args(["pull"])
        .status()
        .await?;
    Command::new("git")
        .current_dir(repo_directory)
        .args(["pull", "--tags"])
        .status()
        .await?;
    Command::new("git")
        .current_dir(repo_directory)
        .args(["push"])
        .status()
        .await?;
    Command::new("git")
        .current_dir(repo_directory)
        .args(["push", "--tags"])
        .status()
        .await?;
    Ok(())
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn update_repos(
    config: &Config,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    let mut handles = Vec::new();

    for repo in &config.workspace_repos {
        let repo_directory = config.workspace_path.join(repo);
        stdout.send(format_sstr!("Process {repo_directory:?}"));

        let stdout = stdout.clone();
        handles.push(tokio::task::spawn(async move {
            process_git_directory(&repo_directory, &stdout).await
        }));
    }
    for repo in &config.setup_files_repos {
        let repo_directory = config.setup_files_path.join(repo);
        stdout.send(format_sstr!("Process {repo_directory:?}"));

        let stdout = stdout.clone();
        handles.push(tokio::task::spawn(async move {
            process_git_directory(&repo_directory, &stdout).await
        }));
    }
    for handle in handles {
        handle.await??;
    }
    Ok(())
}

async fn process_git_directory(
    repo_directory: &Path,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    let p = Command::new("git")
        .current_dir(repo_directory)
        .args(["stash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let status = process_child(p, stdout).await?;
    if !status.success() {
        let code = status.code().ok_or_else(|| format_err!("No status code"))?;
        stdout.send(format_sstr!("git stash failed with {code}"));
        return Ok(());
    }
    let output = Command::new("git")
        .current_dir(repo_directory)
        .args(["branch"])
        .output()
        .await?;
    let lines = StackString::from_utf8_lossy(&output.stdout);
    if lines.find(" main").is_some() {
        process_git_directory_branch(repo_directory, "main").await?;
    } else if lines.find(" master").is_some() {
        process_git_directory_branch(repo_directory, "master").await?;
    }
    let p = Command::new("git")
        .current_dir(repo_directory)
        .args(["gc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let status = process_child(p, stdout).await?;
    if !status.success() {
        let code = status.code().ok_or_else(|| format_err!("No status code"))?;
        stdout.send(format_sstr!("git gc failed with {code}"));
        return Ok(());
    }
    Ok(())
}
