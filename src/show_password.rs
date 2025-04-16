use anyhow::Error;
use clap::Parser;
use ddboline_scripts_rs::{ShowPasswordOpts, config::Config};
use stdout_channel::StdoutChannel;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::new();
    let config = Config::init_config()?;

    let opts = ShowPasswordOpts::parse();
    opts.process(&config, &stdout).await?;

    stdout.close().await?;
    Ok(())
}
