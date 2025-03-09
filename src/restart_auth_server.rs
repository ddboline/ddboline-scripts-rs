use anyhow::Error;
use stack_string::StackString;
use stdout_channel::StdoutChannel;

use ddboline_scripts_rs::{clear_secrets_restart_systemd, config::Config};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();
    let config = Config::init_config()?;

    clear_secrets_restart_systemd(&config, &stdout).await?;
    stdout.close().await?;
    Ok(())
}
