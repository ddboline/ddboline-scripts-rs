#![allow(clippy::semicolon_if_nothing_returned)]

use anyhow::Error;

use ddboline_scripts_rs::config::Config;
use ddboline_scripts_rs::system_stats;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Config::init_config()?;

    system_stats(&config).await?;
    Ok(())
}
