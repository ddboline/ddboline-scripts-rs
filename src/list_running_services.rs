use anyhow::Error;
use stack_string::StackString;
use stdout_channel::StdoutChannel;

use ddboline_scripts_rs::{config::Config, list_running_services};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();
    let config = Config::init_config()?;

    list_running_services(&config, &stdout).await?;
    stdout.close().await?;
    Ok(())
}
