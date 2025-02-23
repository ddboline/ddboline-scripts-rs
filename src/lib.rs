#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::too_many_lines)]

pub mod config;

use anyhow::{format_err, Error};
use log::{debug, error};
use smallvec::SmallVec;
use stack_string::{format_sstr, StackString};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::UNIX_EPOCH,
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

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn check_repo(
    config: &Config,
    stdout: &StdoutChannel<StackString>,
    do_cleanup: bool,
    do_publish: bool,
) -> Result<(), Error> {
    for distro in &config.distros {
        for dir in &config.repo_directories {
            let distro_directory = config.repo_deb_directory.join(distro).join(dir);
            stdout.send(format_sstr!("distro_deb directory {distro_directory:?}"));
            if !distro_directory.exists() {
                continue;
            }
            let mut filemap: HashMap<StackString, BTreeSet<(u64, PathBuf)>> = HashMap::new();
            let mut stream = fs::read_dir(&distro_directory).await?;
            while let Some(entry) = stream.next_entry().await? {
                let path = entry.path();
                if let Some(stem) = path.file_stem() {
                    let stem = stem.to_string_lossy();
                    let parts: SmallVec<[&str; 3]> = stem.split('_').take(3).collect();
                    if parts.is_empty() {
                        continue;
                    }
                    let metadata = entry.metadata().await?;
                    let modified = metadata.modified()?;
                    let mtime = modified.duration_since(UNIX_EPOCH)?.as_secs();
                    let repo_name: StackString = parts[0].into();
                    filemap
                        .entry(repo_name)
                        .or_default()
                        .insert((mtime, path.clone()));
                }
            }
            let mut deb_files = Vec::new();
            for v in filemap.values_mut() {
                while v.len() > 1 {
                    let (_, p) = v
                        .pop_first()
                        .ok_or_else(|| format_err!("unexpected result {v:?}"))?;
                    if do_cleanup {
                        fs::remove_file(&p).await?;
                    }
                }
                if let Some((_, p)) = v.pop_first() {
                    deb_files.push(p);
                }
            }
            if !do_publish {
                continue;
            }
            let repo_directory = config.repo_directory.join(distro).join(dir);
            if !repo_directory.exists() || !config.reprepro_path.exists() || deb_files.is_empty() {
                continue;
            }
            stdout.send(format_sstr!("repo_directory {repo_directory:?}"));
            for d in ["db", "dists", "pool"] {
                let subdir = repo_directory.join(d);
                if subdir.exists() {
                    fs::remove_dir_all(&subdir).await?;
                }
            }
            let repo_directory_str = repo_directory.to_string_lossy();
            let deb_files: Vec<StackString> = deb_files
                .into_iter()
                .map(|p| p.to_string_lossy().into())
                .collect();
            let mut args = vec!["-b", &repo_directory_str, "includedeb", distro];
            args.extend(deb_files.iter().map(StackString::as_str));
            let status = Command::new(&config.reprepro_path)
                .args(&args)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .stdin(Stdio::inherit())
                .status()
                .await?;
            if !status.success() {
                let code = status.code().ok_or_else(|| format_err!("No status code"))?;
                stdout.send(format_sstr!("reprepro failed with {code}"));
                return Ok(());
            }
            if let Some((aws_path, repo_bucket)) = config
                .aws_path
                .as_ref()
                .and_then(|p| config.repo_bucket.as_ref().map(|b| (p, b)))
            {
                let p = Command::new(aws_path)
                    .args([
                        "s3",
                        "sync",
                        "--acl",
                        "public-read",
                        &format_sstr!("{repo_directory_str}/"),
                        &format_sstr!("s3://{repo_bucket}/deb/{distro}/{dir}/"),
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                let status = process_child(p, stdout).await?;
                if !status.success() {
                    let code = status.code().ok_or_else(|| format_err!("No status code"))?;
                    stdout.send(format_sstr!("aws sync failed with {code}"));
                    return Ok(());
                }
                for d in ["db", "dists", "pool"] {
                    let subdir = repo_directory.join(d);
                    if subdir.exists() {
                        fs::remove_dir_all(&subdir).await?;
                    }
                }
            }
        }
    }
    Ok(())
}
