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
use time::{macros::format_description, OffsetDateTime, Duration};
use time_tz::{OffsetDateTimeExt, Tz, timezones::db::UTC};
use std::sync::LazyLock;

use config::{Config, CONFIG_DIR, HOME_DIR};

static LOCAL_TZ: LazyLock<&'static Tz> =
    LazyLock::new(|| time_tz::system::get_timezone().unwrap_or(UTC));
const LOG_DIRS: [&str; 2] = ["crontab", "crontab_aws"];


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

pub async fn authenticate(config: &Config, stdout: &StdoutChannel<StackString>) -> Result<(), Error> {
    let hostname: StackString = get_first_line_of_file(Path::new("/etc/hostname"))
        .await?
        .trim()
        .into();
    stdout.send(format_sstr!("hostname {hostname}"));

    let current_date = OffsetDateTime::now_utc();

    let format = format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]"
    );

    let date_str = current_date.to_timezone(*LOCAL_TZ).format(format)?;
    stdout.send(format_sstr!("date {date_str}"));

    for log_dir in LOG_DIRS {
        let log_path = HOME_DIR.join("log").join(format_sstr!("{log_dir}.log"));
        if log_path.exists() {
            let new_path = HOME_DIR
                .join("log")
                .join(format_sstr!("{log_dir}_{date_str}.log"));
            stdout.send(format_sstr!("mv/gzip {log_path:?} {new_path:?}"));
            fs::rename(&log_path, &new_path).await?;
            let status = Command::new("gzip").args([&new_path]).status().await?;
            if !status.success() {
                let code = status.code().ok_or_else(|| format_err!("No status code"))?;
                stdout.send(format_sstr!("gzip failed with {code}"));
            }
        }
    }
    stdout.send(format_sstr!("sudo apt-get update"));
    let p = Command::new("sudo")
        .args(["apt-get", "update"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let status = process_child(p, &stdout).await?;
    if !status.success() {
        let code = status.code().ok_or_else(|| format_err!("No status code"))?;
        return Err(format_err!("apt-get update failed with {code}"));
    }
    let status = Command::new(&config.send_to_telegram_path)
        .args([
            "-r",
            "ddboline",
            "-m",
            &format_sstr!("{hostname} has updated"),
        ])
        .status()
        .await?;
    if !status.success() {
        let code = status.code().ok_or_else(|| format_err!("No status code"))?;
        return Err(format_err!("send-to-telegram failed with {code}"));
    }
    let status = Command::new("sudo")
        .args(["apt-get", "dist-upgrade"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .await?;
    if !status.success() {
        let code = status.code().ok_or_else(|| format_err!("No status code"))?;
        return Err(format_err!("apt-get dist-upgrade failed with {code}"));
    }
    if hostname == "dilepton-tower" {
        let status = Command::new("sudo")
            .args(["modprobe", "vboxdrv"])
            .status()
            .await?;
        if !status.success() {
            let code = status.code().ok_or_else(|| format_err!("No status code"))?;
            return Err(format_err!("modprobe vboxdrv failed with {code}"));
        }
        let postgres_toml = CONFIG_DIR.join("backup_app_rust").join("postgres.toml");
        if postgres_toml.exists() {
            let postgres_toml = postgres_toml.to_string_lossy();
            let p = Command::new(&config.backup_app_path)
                .args(["backup", "-f", &postgres_toml])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let status = process_child(p, &stdout).await?;
            if !status.success() {
                let code = status.code().ok_or_else(|| format_err!("No status code"))?;
                return Err(format_err!(
                    "backup_app_rust postgres.toml failed with {code}"
                ));
            }
        }
    }
    let postgres_local_toml = CONFIG_DIR
        .join("backup_app_rust")
        .join("postgres_local.toml");
    if postgres_local_toml.exists() {
        let postgres_local_toml = postgres_local_toml.to_string_lossy();
        let p = Command::new(&config.backup_app_path)
            .args(["backup", "-f", &postgres_local_toml])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let status = process_child(p, &stdout).await?;
        if !status.success() {
            let code = status.code().ok_or_else(|| format_err!("No status code"))?;
            return Err(format_err!(
                "backup_app_rust postgres_local.toml failed with {code}"
            ));
        }
    }
    if config.dropbox_path.exists() {
        let status = Command::new(&config.dropbox_path)
            .args(["start"])
            .status()
            .await?;
        if !status.success() {
            let code = status.code().ok_or_else(|| format_err!("No status code"))?;
            return Err(format_err!("dropbox failed with {code}"));
        }
    }
    let status = Command::new(&config.send_to_telegram_path)
        .args([
            "-r",
            "ddboline",
            "-m",
            &format_sstr!("{hostname} has finished"),
        ])
        .status()
        .await?;
    if !status.success() {
        let code = status.code().ok_or_else(|| format_err!("No status code"))?;
        return Err(format_err!("send-to-telegram failed with {code}"));
    }
    update_repos(&config, &stdout).await?;

    Ok(())
}

pub async fn system_stats(config: &Config) -> Result<(), Error> {
    let weather = if config.weather_util_path.exists() {
        Some(
            Command::new(&config.weather_util_path)
                .args(["-z", "11106"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        )
    } else {
        None
    };
    let calendar = if config.calendar_app_path.exists() {
        Some(
            Command::new(&config.calendar_app_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        )
    } else {
        None
    };

    let freq: i64 = get_first_line_of_file(&config.frequency_path)
        .await?
        .trim()
        .parse()
        .unwrap_or(0);
    let temp: i64 = get_first_line_of_file(&config.temperature_path)
        .await?
        .trim()
        .parse()
        .unwrap_or(0);
    let freq = freq / 1000;
    let temp = temp / 1000;
    let uptime: f64 = get_first_line_of_file(&config.uptime_path)
        .await?
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let uptime = Duration::seconds_f64(uptime);
    let mut uptime_str = Vec::new();
    let weeks = uptime.whole_weeks();
    let days = uptime.whole_days() % 7;
    let hours = uptime.whole_hours() % 24;
    let minutes = uptime.whole_minutes() % 60;
    let seconds = uptime.whole_seconds() % 60;
    let subseconds = uptime.as_seconds_f64() % 1f64;
    let subseconds = &format_sstr!("{subseconds:.3}")[1..];
    if weeks > 0 {
        uptime_str.push(format_sstr!("{weeks} weeks"));
    }
    if days > 0 {
        uptime_str.push(format_sstr!("{days} days"));
    }
    uptime_str.push(format_sstr!(
        "{hours:02}:{minutes:02}:{seconds:02}{subseconds}"
    ));
    let uptime_seconds = uptime.whole_seconds();
    let uptime_str = uptime_str.join(" ");

    println!("Uptime {uptime_seconds} seconds or {uptime_str}");
    println!("Temperature {temp} C  CpuFreq {freq} MHz");

    if let Some(weather) = weather {
        println!("\nWeather:");
        let output = weather.wait_with_output().await?;
        let output = StackString::from_utf8_lossy(&output.stdout);
        println!("{output}");
    }
    if let Some(calendar) = calendar {
        println!("\nAgenda:");
        let output = calendar.wait_with_output().await?;
        let output = StackString::from_utf8_lossy(&output.stdout);
        println!("{output}");
    }
    Ok(())
}