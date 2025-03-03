use anyhow::Error;
use stdout_channel::StdoutChannel;
use stack_string::StackString;

use ddboline_scripts_rs::list_running_services;
use ddboline_scripts_rs::config::Config;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();
    let config = Config::init_config()?;

    list_running_services(&config, &stdout).await?;
    stdout.close().await?;
    Ok(())
}