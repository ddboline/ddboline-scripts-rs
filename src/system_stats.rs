#![allow(clippy::semicolon_if_nothing_returned)]

use anyhow::Error;
use stack_string::{format_sstr, StackString};
use std::{path::Path, process::Stdio};
use time::Duration;
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use ddboline_scripts_rs::config::Config;

async fn get_first_line_of_file(fpath: &Path) -> Result<String, Error> {
    let mut buf = String::new();
    if fpath.exists() {
        if let Ok(f) = fs::File::open(fpath).await {
            let mut buf_read = BufReader::new(f);
            buf_read.read_line(&mut buf).await?;
        }
    }
    Ok(buf)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Config::init_config()?;

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
