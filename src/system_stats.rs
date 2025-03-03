#![allow(clippy::semicolon_if_nothing_returned)]

use anyhow::Error;
use stack_string::StackString;
use stdout_channel::StdoutChannel;

use ddboline_scripts_rs::{config::Config, system_stats};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();
    let config = Config::init_config()?;

    system_stats(&config, &stdout).await?;

    stdout.close().await?;
    Ok(())
}
