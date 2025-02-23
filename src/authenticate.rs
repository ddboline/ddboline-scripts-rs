#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::too_many_lines)]

use anyhow::{format_err, Error};
use stack_string::{format_sstr, StackString};
use std::{path::Path, process::Stdio, sync::LazyLock};
use stdout_channel::StdoutChannel;
use time::{macros::format_description, OffsetDateTime};
use time_tz::{timezones::db::UTC, OffsetDateTimeExt, Tz};
use tokio::{fs, process::Command};

use ddboline_scripts_rs::{
    config::{Config, CONFIG_DIR, HOME_DIR},
    get_first_line_of_file, process_child, update_repos,
};

static LOCAL_TZ: LazyLock<&'static Tz> =
    LazyLock::new(|| time_tz::system::get_timezone().unwrap_or(UTC));
const LOG_DIRS: [&str; 2] = ["crontab", "crontab_aws"];

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();

    let config = Config::init_config()?;
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
    stdout.close().await?;
    Ok(())
}
