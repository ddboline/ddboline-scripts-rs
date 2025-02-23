#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::too_many_lines)]

use anyhow::Error;
use stack_string::StackString;
use stdout_channel::StdoutChannel;

use ddboline_scripts_rs::{authenticate, config::Config};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let stdout = StdoutChannel::<StackString>::new();

    let config = Config::init_config()?;

    authenticate(&config, &stdout).await?;

    stdout.close().await?;
    Ok(())
}
