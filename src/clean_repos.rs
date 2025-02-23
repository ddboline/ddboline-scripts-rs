use anyhow::Error;
use stack_string::StackString;
use stdout_channel::StdoutChannel;

use ddboline_scripts_rs::{check_repo, config::Config};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();
    let config = Config::init_config()?;

    check_repo(&config, &stdout, true, false).await?;
    Ok(())
}
