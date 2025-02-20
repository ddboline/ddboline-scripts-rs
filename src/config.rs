use anyhow::Error;
use serde::Deserialize;
use stack_string::StackString;
use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

pub static CONFIG_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::config_dir().expect("No CONFIG directory"));
pub static HOME_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::home_dir().expect("No HOME directory"));
static BIN_DIR: LazyLock<&Path> = LazyLock::new(|| Path::new("/usr/bin"));

#[derive(Default, Debug, Deserialize, PartialEq)]
pub struct ConfigInner {
    #[serde(default = "Vec::new")]
    pub systemd_services: Vec<StackString>,
    #[serde(default = "Vec::new")]
    pub workspace_repos: Vec<StackString>,
    #[serde(default = "default_workspace_path")]
    pub workspace_path: PathBuf,
    #[serde(default = "Vec::new")]
    pub setup_files_repos: Vec<StackString>,
    #[serde(default = "default_setup_files_path")]
    pub setup_files_path: PathBuf,
    #[serde(default = "default_send_to_telegram_path")]
    pub send_to_telegram_path: PathBuf,
    #[serde(default = "default_backup_app_path")]
    pub backup_app_path: PathBuf,
    #[serde(default = "default_dropbox_path")]
    pub dropbox_path: PathBuf,
    #[serde(default = "default_weather_util_path")]
    pub weather_util_path: PathBuf,
    #[serde(default = "default_calendar_app_path")]
    pub calendar_app_path: PathBuf,
    #[serde(default = "default_frequency_path")]
    pub frequency_path: PathBuf,
    #[serde(default = "default_temperature_path")]
    pub temperature_path: PathBuf,
    #[serde(default = "default_uptime_path")]
    pub uptime_path: PathBuf,
    #[serde(default = "Vec::new")]
    pub distros: Vec<StackString>,
    #[serde(default = "Vec::new")]
    pub repo_directories: Vec<StackString>,
    #[serde(default = "Vec::new")]
    pub architectures: Vec<StackString>,
}

fn default_workspace_path() -> PathBuf {
    HOME_DIR.join("Workspace")
}

fn default_setup_files_path() -> PathBuf {
    HOME_DIR.join("setup_files").join("build")
}

fn default_send_to_telegram_path() -> PathBuf {
    BIN_DIR.join("send-to-telegram")
}

fn default_backup_app_path() -> PathBuf {
    BIN_DIR.join("backup-app-rust")
}

fn default_dropbox_path() -> PathBuf {
    BIN_DIR.join("dropbox")
}

fn default_weather_util_path() -> PathBuf {
    BIN_DIR.join("weather-util-rust")
}

fn default_calendar_app_path() -> PathBuf {
    BIN_DIR.join("calendar-app-rust")
}

fn default_frequency_path() -> PathBuf {
    Path::new("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq").to_path_buf()
}

fn default_temperature_path() -> PathBuf {
    Path::new("/sys/devices/virtual/thermal/thermal_zone0/temp").to_path_buf()
}

fn default_uptime_path() -> PathBuf {
    Path::new("/proc/uptime").to_path_buf()
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Config(Arc<ConfigInner>);

impl Deref for Config {
    type Target = ConfigInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_inner(inner: ConfigInner) -> Self {
        Self(Arc::new(inner))
    }

    pub fn init_config() -> Result<Self, Error> {
        let fname = Path::new("config.env");

        let env_file = if fname.exists() {
            fname.to_path_buf()
        } else {
            CONFIG_DIR.join("ddboline_scripts_rs").join("config.env")
        };

        dotenvy::dotenv().ok();
        if env_file.exists() {
            dotenvy::from_path(&env_file).ok();
        }

        let conf: ConfigInner = envy::from_env()?;

        Ok(Self::from_inner(conf))
    }
}
