#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::too_many_lines)]

pub mod config;

use anyhow::{Error, format_err};
use checksums::{Algorithm, hash_file};
use clap::Parser;
use log::error;
use rand::{
    distr::{Alphanumeric, Distribution, SampleString, Uniform},
    rng as thread_rng,
};
use smallvec::SmallVec;
use stack_string::{MAX_INLINE, StackString, format_sstr};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    fmt,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::LazyLock,
    time::UNIX_EPOCH,
};
use stdout_channel::StdoutChannel;
use time::{Duration, OffsetDateTime, macros::format_description};
use time_tz::{OffsetDateTimeExt, Tz, timezones::db::UTC};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    task::{JoinHandle, spawn},
};

use config::{CONFIG_DIR, Config, HOME_DIR};

static LOCAL_TZ: LazyLock<&'static Tz> =
    LazyLock::new(|| time_tz::system::get_timezone().unwrap_or(UTC));
const LOG_DIRS: [&str; 3] = ["crontab", "crontab_aws", "crontab_root"];

fn get_lower_case() -> SmallVec<[char; 26]> {
    (0..26).map(|v| (b'a' + v as u8) as char).collect()
}

fn get_upper_case() -> SmallVec<[char; 26]> {
    (0..26).map(|v| (b'A' + v as u8) as char).collect()
}

fn get_digits() -> SmallVec<[char; 10]> {
    (0..10).map(|v| (b'0' + v as u8) as char).collect()
}

fn get_special_chars() -> SmallVec<[char; 15]> {
    "~@#$%^&*,.()|/?".chars().collect()
}

#[must_use]
fn get_random_string(n: usize) -> StackString {
    let mut rng = thread_rng();
    if n > MAX_INLINE {
        Alphanumeric.sample_string(&mut rng, n).into()
    } else {
        let buf: SmallVec<[u8; MAX_INLINE]> = Alphanumeric.sample_iter(&mut rng).take(n).collect();
        StackString::from_utf8_lossy(&buf[0..n])
    }
}

fn get_random_string_from_chars(n: usize, chars: &[char]) -> StackString {
    let mut rng = thread_rng();
    let uniform = Uniform::try_from(0..chars.len()).expect("failed to create uniform distribution");
    uniform
        .sample_iter(&mut rng)
        .map(|i| chars[i])
        .take(n)
        .collect()
}

async fn get_first_line_of_file(fpath: &Path) -> Result<StackString, Error> {
    if !fpath.exists() {
        return Ok(StackString::new());
    }
    let metadata_len = fs::metadata(&fpath).await?.len();
    if metadata_len < 1024 {
        let mut buf = fs::read_to_string(&fpath).await?;
        let endbyte = buf.find('\n').unwrap_or(buf.len());
        buf.truncate(endbyte);
        let buf = buf.trim().into();
        Ok(buf)
    } else {
        let mut buf = String::new();
        if let Ok(f) = fs::File::open(fpath).await {
            let mut buf_read = BufReader::new(f);
            buf_read.read_line(&mut buf).await?;
        }
        let buf = buf.trim().into();
        Ok(buf)
    }
}

async fn process_reader(
    mut reader: BufReader<impl AsyncRead + Unpin>,
    eol: u8,
    f: impl Fn(&[u8]),
) -> Result<(), Error> {
    let mut buf = Vec::new();
    while let Ok(bytes) = reader.read_until(eol, &mut buf).await {
        if bytes > 0 {
            f(&buf);
        } else {
            break;
        }
        buf.clear();
    }
    Ok(())
}

async fn output_to_stdout(
    reader: BufReader<impl AsyncRead + Unpin>,
    eol: u8,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    process_reader(reader, eol, |v| {
        stdout.send(String::from_utf8_lossy(v).trim_end_matches('\n'));
    })
    .await
}

async fn output_to_stderr(
    reader: BufReader<impl AsyncRead + Unpin>,
    eol: u8,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    process_reader(reader, eol, |v| {
        stdout.send_err(String::from_utf8_lossy(v).trim_end_matches('\n'));
    })
    .await
}

async fn process_child(
    mut process: Child,
    stdout_channel: &StdoutChannel<StackString>,
    label: &str,
) -> Result<ExitStatus, Error> {
    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| format_err!("No Stdout"))?;
    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| format_err!("No Stderr"))?;
    let stdout_task: JoinHandle<Result<(), Error>> = {
        let reader = BufReader::new(stdout);
        let stdout_channel = stdout_channel.clone();
        spawn(async move { output_to_stdout(reader, b'\n', &stdout_channel).await })
    };
    let stderr_task: JoinHandle<Result<(), Error>> = {
        let reader = BufReader::new(stderr);
        let stdout_channel = stdout_channel.clone();
        spawn(async move { output_to_stderr(reader, b'\n', &stdout_channel).await })
    };
    let status = process.wait().await?;
    stdout_task.await??;
    stderr_task.await??;
    if status.success() {
        Ok(status)
    } else {
        let code = status
            .code()
            .ok_or_else(|| format_err!("No status code 0 {label}"))?;
        stdout_channel.send(format_sstr!("{label} failed with {code}"));
        Err(format_err!("{label} failed with {code}"))
    }
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
    process_child(p, stdout, "git stash").await?;
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
    process_child(p, stdout, "git gc").await?;
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
            if !distro_directory.exists() {
                continue;
            }
            let devel_directory = config
                .repo_deb_directory
                .join(distro)
                .join(format_sstr!("devel_{dir}"));
            if devel_directory.exists() {
                let mut deb_files = Vec::new();
                let mut log_files = Vec::new();
                let mut stream = fs::read_dir(&devel_directory).await?;
                while let Some(entry) = stream.next_entry().await? {
                    let path = entry.path();
                    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
                        if ext == "deb" {
                            deb_files.push(path);
                        } else if ext == "log" {
                            log_files.push(path);
                        }
                    }
                }

                for path in log_files {
                    let buf = fs::read_to_string(&path).await?;
                    let lines = buf.split('\n').count();
                    for line in buf.split('\n').skip(lines - 20) {
                        stdout.send(StackString::from(line));
                    }
                    if do_cleanup {
                        fs::remove_file(&path).await?;
                    }
                }

                for path in deb_files {
                    if let Some(filename) = path.file_name().and_then(OsStr::to_str) {
                        stdout.send(format_sstr!("devel {filename}"));
                        if do_cleanup {
                            let final_path = distro_directory.join(filename);
                            fs::rename(path, final_path).await?;
                        }
                    }
                }
            }
            stdout.send(format_sstr!("distro_deb directory {distro}"));
            let mut filemap: HashMap<StackString, BTreeSet<(u64, StackString)>> = HashMap::new();
            let mut stream = fs::read_dir(&distro_directory).await?;
            while let Some(entry) = stream.next_entry().await? {
                let path = entry.path();
                if let Some((path_str, stem_str)) =
                    path.to_str().zip(path.file_stem().and_then(OsStr::to_str))
                {
                    let parts: SmallVec<[&str; 3]> = stem_str.split('_').take(3).collect();
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
                        .insert((mtime, path_str.into()));
                }
            }
            let mut deb_files = Vec::new();
            for v in filemap.values_mut() {
                while v.len() > 1 {
                    let (_, p) = v
                        .pop_first()
                        .ok_or_else(|| format_err!("unexpected result {v:?}"))?;
                    stdout.send(format_sstr!("duplicate {p}"));
                    if do_cleanup {
                        stdout.send(format_sstr!("remove {p}"));
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
                let code = status
                    .code()
                    .ok_or_else(|| format_err!("No status code 1"))?;
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
                process_child(p, stdout, "aws sync").await?;
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

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn authenticate(
    config: &Config,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    let hostname = get_first_line_of_file(Path::new("/etc/hostname")).await?;
    stdout.send(format_sstr!("hostname {hostname}"));

    let user = std::env::var("USER")?;

    let current_date = OffsetDateTime::now_utc();

    let format = format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]"
    );

    let date_str = current_date.to_timezone(*LOCAL_TZ).format(format)?;
    stdout.send(format_sstr!("date {date_str}"));

    let root_log_path = HOME_DIR.join("log").join(format_sstr!("crontab_root.log"));
    if root_log_path.exists() {
        let final_path = root_log_path.to_string_lossy();
        let status = Command::new("sudo")
            .args(["chown", &format_sstr!("{user}:{user}"), &final_path])
            .status()
            .await?;
        if !status.success() {
            let code = status
                .code()
                .ok_or_else(|| format_err!("No status code 2"))?;
            stdout.send(format_sstr!("copy failed with {code}"));
        }
    }

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
                let code = status
                    .code()
                    .ok_or_else(|| format_err!("No status code 3"))?;
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
    process_child(p, stdout, "apt-get update").await?;
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
        let code = status
            .code()
            .ok_or_else(|| format_err!("No status code 4"))?;
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
        let code = status
            .code()
            .ok_or_else(|| format_err!("No status code 5"))?;
        error!("apt-get dist-upgrade failed with {code}");
    }
    if hostname == "dilepton-tower" {
        // let status = Command::new("sudo")
        //     .args(["modprobe", "vboxdrv"])
        //     .status()
        //     .await?;
        // if !status.success() {
        //     let code = status
        //         .code()
        //         .ok_or_else(|| format_err!("No status code 6"))?;
        //     return Err(format_err!("modprobe vboxdrv failed with {code}"));
        // }
        let postgres_toml = CONFIG_DIR.join("backup_app_rust").join("postgres.toml");
        if postgres_toml.exists() {
            let postgres_toml = postgres_toml.to_string_lossy();
            let p = Command::new(&config.backup_app_path)
                .args(["backup", "-f", &postgres_toml])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            process_child(p, stdout, "backup_app_rust postgres.toml").await?;
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
        process_child(p, stdout, "backup_app_rust postgres_local.toml").await?;
    }
    if config.dropbox_path.exists() {
        let status = Command::new(&config.dropbox_path)
            .args(["start"])
            .status()
            .await?;
        if !status.success() {
            let code = status
                .code()
                .ok_or_else(|| format_err!("No status code 7"))?;
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
        let code = status
            .code()
            .ok_or_else(|| format_err!("No status code 8"))?;
        return Err(format_err!("send-to-telegram failed with {code}"));
    }
    update_repos(config, stdout).await?;
    if hostname != "dilepton-tower" {
        let postgres_toml = CONFIG_DIR.join("backup_app_rust").join("postgres.toml");
        if postgres_toml.exists() {
            let postgres_toml = postgres_toml.to_string_lossy();
            let p = Command::new(&config.backup_app_path)
                .args([
                    "restore",
                    "-f",
                    &postgres_toml,
                    "-k",
                    "movie_collection_rust",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            process_child(p, stdout, "backup_app_rust postgres.toml").await?;
        }
    }

    Ok(())
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn system_stats(
    config: &Config,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
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
        .parse()
        .unwrap_or(0);
    let temp: i64 = get_first_line_of_file(&config.temperature_path)
        .await?
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

    stdout.send(format_sstr!(
        "Uptime {uptime_seconds} seconds or {uptime_str}"
    ));
    stdout.send(format_sstr!("Temperature {temp} C  CpuFreq {freq} MHz"));

    if let Some(weather) = weather {
        stdout.send(format_sstr!("\nWeather:"));
        let output = weather.wait_with_output().await?;
        let output = StackString::from_utf8_lossy(&output.stdout);
        stdout.send(format_sstr!("{output}"));
    }
    if let Some(calendar) = calendar {
        stdout.send(format_sstr!("\nAgenda:"));
        let output = calendar.wait_with_output().await?;
        let output = StackString::from_utf8_lossy(&output.stdout);
        stdout.send(format_sstr!("{output}"));
    }
    Ok(())
}

#[derive(Debug)]
struct SystemctlStatus<'a> {
    unit: &'a str,
    load: &'a str,
    active: &'a str,
    sub: &'a str,
    description: &'a str,
}

impl fmt::Display for SystemctlStatus<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{unit} {load} {active} {sub} {description}",
            unit = self.unit,
            load = self.load,
            active = self.active,
            sub = self.sub,
            description = self.description
        )
    }
}

impl<'a> SystemctlStatus<'a> {
    fn from_systemctl_line(
        line: &'a str,
        units: &'a HashMap<StackString, impl AsRef<str>>,
    ) -> Option<Self> {
        let mut it = line.split_ascii_whitespace();
        let service = it.next()?;

        let mut start_index = line.find(service)?;

        let unit = units.get(service)?.as_ref();
        let load = it.next()?;

        start_index += line.split_at_checked(start_index)?.1.find(load)?;

        let active = it.next()?;

        start_index += line.split_at_checked(start_index)?.1.find(active)?;

        let sub = it.next()?;

        start_index += line.split_at_checked(start_index)?.1.find(sub)?;

        let next = it.next()?;

        start_index += line.split_at_checked(start_index)?.1.find(next)?;

        let description = line.split_at_checked(start_index)?.1.trim();

        Some(SystemctlStatus {
            unit,
            load,
            active,
            sub,
            description,
        })
    }
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn list_running_services(
    config: &Config,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    let units: HashMap<StackString, &str> = config
        .systemd_services
        .iter()
        .map(|s| (format_sstr!("{s}.service"), s.as_str()))
        .collect();
    let output = Command::new("systemctl").output().await?;
    let output = StackString::from_utf8_lossy(&output.stdout);

    let statuses: Vec<SystemctlStatus> = output
        .split('\n')
        .filter_map(|line| SystemctlStatus::from_systemctl_line(line, &units))
        .collect();

    let max_unit = statuses.iter().map(|s| s.unit.len()).max().unwrap_or(0);
    let max_active = statuses.iter().map(|s| s.active.len()).max().unwrap_or(0);
    let max_description = statuses
        .iter()
        .map(|s| s.description.len())
        .max()
        .unwrap_or(0);

    for status in statuses {
        fn fixed_size(input: &str, size: usize) -> StackString {
            let mut s = StackString::new();
            s.push_str(input);
            while s.len() < size {
                s.push_str(" ");
            }
            s
        }

        let unit = fixed_size(status.unit, max_unit + 1);
        let active = fixed_size(status.active, max_active + 1);
        let description = fixed_size(status.description, max_description + 1);

        stdout.send(format_sstr!("{unit} {description} {active} "));
    }
    Ok(())
}

/// # Errors
/// Return error if callback function returns error after timeout
pub async fn clear_secrets_restart_systemd(
    config: &Config,
    stdout: &StdoutChannel<StackString>,
) -> Result<(), Error> {
    let services: Vec<_> = config
        .systemd_services
        .iter()
        .filter_map(|s| {
            if s.as_str() == "nginx" {
                None
            } else {
                Some(s.as_str())
            }
        })
        .collect();
    if !services.contains(&"auth-server-rust") {
        stdout.send_err("auth server not found");
        return Ok(());
    }
    if config.secret_path.exists() {
        let p = &config.secret_path;
        stdout.send(format_sstr!("remove {p:?}"));
        fs::remove_file(p).await?;
    }
    if config.jwt_secret_path.exists() {
        let p = &config.jwt_secret_path;
        stdout.send(format_sstr!("remove {p:?}"));
        fs::remove_file(p).await?;
    }
    stdout.send(format_sstr!("restart auth-server-rust"));
    let status = Command::new("sudo")
        .args(["systemctl", "restart", "auth-server-rust"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .await?;
    if !status.success() {
        let code = status
            .code()
            .ok_or_else(|| format_err!("No status code 9"))?;
        error!("systemctl restart auth-server-rust failed with code {code}");
    }
    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    for service in services {
        stdout.send(format_sstr!("restart {service}"));
        let status = Command::new("sudo")
            .args(["systemctl", "restart", service])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .stdin(Stdio::inherit())
            .status()
            .await?;
        if !status.success() {
            let code = status
                .code()
                .ok_or_else(|| format_err!("No status code 10"))?;
            error!("systemctl restart {service} failed with code {code}");
        }
    }
    Ok(())
}

async fn get_current_password_file(config: &Config) -> Result<Option<PathBuf>, Error> {
    let mut current_password_file: Option<StackString> = None;
    let mut stream = fs::read_dir(&config.password_directory).await?;
    while let Some(entry) = stream.next_entry().await? {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(OsStr::to_str) {
            if ext != "asc" {
                continue;
            }
            if let Some(filename) = path.file_name().and_then(OsStr::to_str)
                && filename.starts_with("passwords")
                && filename.ends_with(".txt.asc")
                && (current_password_file.is_none()
                    || (current_password_file.as_ref().map(StackString::as_str) < Some(filename)))
            {
                current_password_file.replace(filename.into());
            }
        }
    }
    Ok(current_password_file.map(|f| config.password_directory.join(f)))
}

async fn decrypt_password_file(
    input: Option<PathBuf>,
    config: &Config,
    stdout: &StdoutChannel<StackString>,
) -> Result<PathBuf, Error> {
    let date = format_sstr!("{}", OffsetDateTime::now_utc().date());
    let current_password_file = if let Some(input) = input {
        if !input.exists() {
            return Err(format_err!("Password File Not Found"));
        }
        input
    } else {
        get_current_password_file(config)
            .await?
            .ok_or_else(|| format_err!("no password file found"))?
    };
    let current_password_str = current_password_file.to_string_lossy();
    stdout.send(format_sstr!(
        "current_password_file {current_password_file:?}"
    ));
    let f_name = format_sstr!("/tmp/passwords_{date}_{}.txt", get_random_string(16));
    let status = Command::new("gpg")
        .args(["--quiet", "--output", &f_name, "-d", &current_password_str])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .await?;
    if !status.success() {
        let code = status
            .code()
            .ok_or_else(|| format_err!("No status code 11"))?;
        let message = format_sstr!("gpg -d failed with {code}");
        stdout.send(message.clone());
        return Err(format_err!("{message}"));
    }
    Ok(Path::new(&f_name).to_path_buf())
}

#[derive(Parser, Debug, Clone)]
pub enum ShowPasswordOpts {
    Create {
        #[clap(short, long)]
        number_of_characters: Option<usize>,
    },
    Show {
        #[clap(short, long)]
        input: Option<PathBuf>,
        #[clap(short, long)]
        query: Option<StackString>,
    },
    Edit {
        #[clap(short, long)]
        input: Option<PathBuf>,
        #[clap(short, long)]
        output: Option<PathBuf>,
    },
    Send {
        #[clap(short, long)]
        input: Option<PathBuf>,
        #[clap(short, long)]
        query: StackString,
    },
}

impl ShowPasswordOpts {
    /// # Errors
    /// Returns error for various
    pub async fn process(
        self,
        config: &Config,
        stdout: &StdoutChannel<StackString>,
    ) -> Result<(), Error> {
        match self {
            Self::Create {
                number_of_characters,
            } => {
                let number_of_characters = number_of_characters.unwrap_or(16);
                let upper = get_upper_case();
                let lower = get_lower_case();
                let digits = get_digits();
                let special = get_special_chars();
                let upper_lower = [upper.as_slice(), lower.as_slice()].concat();
                let upper_lower_digits = [upper_lower.as_slice(), digits.as_slice()].concat();
                let all_chars = [upper_lower_digits.as_slice(), special.as_slice()].concat();

                let upper_pwd = get_random_string_from_chars(number_of_characters, &upper);
                let lower_pwd = get_random_string_from_chars(number_of_characters, &lower);
                let upper_lower_pwd =
                    get_random_string_from_chars(number_of_characters, &upper_lower);
                let upper_lower_digits_pwd =
                    get_random_string_from_chars(number_of_characters, &upper_lower_digits);
                let chars_pwd = get_random_string_from_chars(number_of_characters, &all_chars);
                stdout.send(
                    [
                        upper_pwd,
                        lower_pwd,
                        upper_lower_pwd,
                        upper_lower_digits_pwd,
                        chars_pwd,
                    ]
                    .join("\n"),
                );
            }
            Self::Show { input, query } => {
                let f_name = decrypt_password_file(input, config, stdout).await?;
                if let Some(query) = query {
                    let f = fs::File::open(&f_name).await?;
                    let mut reader = BufReader::new(f);
                    let mut buf = String::new();
                    while let Ok(bytes) = reader.read_line(&mut buf).await {
                        if bytes == 0 {
                            break;
                        }
                        if buf.contains(query.as_str()) {
                            let s = format_sstr!("\n{buf}");
                            stdout.send(s);
                            break;
                        }
                        buf.clear();
                    }
                } else {
                    let s: StackString = fs::read_to_string(&f_name).await?.into();
                    stdout.send(s);
                }
                fs::remove_file(&f_name).await?;
            }
            Self::Send { input, query } => {
                let f_name = decrypt_password_file(input, config, stdout).await?;
                let f = fs::File::open(&f_name).await?;
                let mut reader = BufReader::new(f);
                let mut buf = String::new();
                while let Ok(bytes) = reader.read_line(&mut buf).await {
                    if bytes == 0 {
                        break;
                    }
                    if buf.contains(query.as_str()) {
                        let status = Command::new(&config.send_to_telegram_path)
                            .args(["-r", "ddboline", "-m", &buf])
                            .status()
                            .await?;
                        if !status.success() {
                            let code = status
                                .code()
                                .ok_or_else(|| format_err!("No status code 12"))?;
                            return Err(format_err!("send-to-telegram failed with {code}"));
                        }

                        let s = format_sstr!("{buf}");
                        stdout.send(s);
                        break;
                    }
                    buf.clear();
                }
                fs::remove_file(&f_name).await?;
            }
            Self::Edit { input, output } => {
                let gpg_user = config
                    .gpg_user
                    .as_ref()
                    .ok_or_else(|| format_err!("No gpg user found"))?;
                let gpg_key = config
                    .gpg_key
                    .as_ref()
                    .ok_or_else(|| format_err!("No gpg key found"))?;
                let f_path = decrypt_password_file(input, config, stdout).await?;
                let f_hash = hash_file(&f_path, Algorithm::BLAKE3);
                let f_name = f_path.to_string_lossy();
                let status = Command::new("emacs")
                    .args(["-nw", &f_name])
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .stdin(Stdio::inherit())
                    .status()
                    .await?;
                if !status.success() {
                    let code = status
                        .code()
                        .ok_or_else(|| format_err!("No status code 13"))?;
                    fs::remove_file(&f_path).await?;
                    return Err(format_err!("emacs failed with {code}"));
                }
                let new_hash = hash_file(&f_path, Algorithm::BLAKE3);
                if f_hash != new_hash {
                    let new_password_file = if let Some(output) = output {
                        output
                    } else {
                        let date = format_sstr!("{}", OffsetDateTime::now_utc().date());
                        config
                            .password_directory
                            .join(format_sstr!("passwords_{date}.txt.asc"))
                    };
                    let new_password_str = new_password_file.to_string_lossy();
                    let status = Command::new("gpg")
                        .args([
                            "-aes",
                            "-r",
                            gpg_key.as_str(),
                            "-u",
                            gpg_user.as_str(),
                            "--output",
                            &new_password_str,
                            &f_name,
                        ])
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .stdin(Stdio::inherit())
                        .status()
                        .await?;
                    if !status.success() {
                        let code = status
                            .code()
                            .ok_or_else(|| format_err!("No status code 14"))?;
                        fs::remove_file(&f_path).await?;
                        return Err(format_err!("gpg encrypt failed with {code}"));
                    }
                }
                fs::remove_file(&f_path).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::{
        get_digits, get_first_line_of_file, get_lower_case, get_random_string,
        get_random_string_from_chars, get_special_chars, get_upper_case,
    };

    #[tokio::test]
    async fn test_get_first_line_of_file() {
        let test_file = Path::new("LICENSE");

        assert!(test_file.exists());

        let buf = get_first_line_of_file(test_file).await.unwrap();

        assert_eq!(buf, "MIT License");
    }

    #[test]
    fn test_get_random_string() {
        let s = get_random_string(12);
        assert_eq!(s.len(), 12);
        println!("{s}");
    }

    #[test]
    fn test_get_random_string_from_chars() {
        let upper = get_upper_case();
        let lower = get_lower_case();
        let digits = get_digits();
        let special = get_special_chars();

        let s = get_random_string_from_chars(16, &upper);
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| upper.contains(&c)));
        println!("{s}");

        let s = get_random_string_from_chars(16, &lower);
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| lower.contains(&c)));
        println!("{s}");

        let upper_lower = [upper.as_slice(), lower.as_slice()].concat();
        let s = get_random_string_from_chars(16, &upper_lower);
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| upper_lower.contains(&c)));
        println!("{s}");

        let chars = [
            upper.as_slice(),
            lower.as_slice(),
            digits.as_slice(),
            special.as_slice(),
        ]
        .concat();

        let s = get_random_string_from_chars(16, &chars);
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| chars.contains(&c)));
        println!("{s}");
    }
}
