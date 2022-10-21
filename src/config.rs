use directories::ProjectDirs;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs::{create_dir_all, File},
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    str::FromStr,
};
use tokio::net::lookup_host;

pub fn default_path() -> PathBuf {
    let config_file_name = if cfg!(debug_assertions) {
        "config-devel.toml"
    } else {
        "config.toml"
    };

    if let Some(project_dirs) = ProjectDirs::from("", "", "kueue") {
        return project_dirs.config_dir().join(config_file_name);
    }
    config_file_name.into()
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Config {
    pub log_level: String,
    pub server_bind_address: String,
    pub server_address: String,
    pub server_port: u16,
    pub shared_secret: String,
    pub restart_workers: RestartWorkers,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RestartWorkers {
    pub ssh_user: String,
    pub hostnames: String,
}

impl Config {
    pub fn new() -> Result<Self, config::ConfigError> {
        let config_path: String = default_path().to_string_lossy().into();

        // TODO: Raise default levels when more mature.
        let default_log_level = if cfg!(debug_assertions) {
            "trace"
        } else {
            "info"
        };

        let random_secret: String = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(30)
            .map(char::from)
            .collect();

        let s = config::Config::builder()
            .set_default("log_level", default_log_level)?
            .set_default("server_bind_address", "0.0.0.0")?
            .set_default("server_address", "localhost")?
            .set_default("server_port", 11236)?
            .set_default("shared_secret", random_secret)?
            .add_source(config::File::with_name(&config_path).required(false))
            .build()?;

        s.try_deserialize()
    }

    pub fn create_default_config(&self) -> Result<(), Box<dyn Error>> {
        let config_path = default_path();
        let toml = toml::to_vec(&self)?;

        if let Some(config_dir) = config_path.parent() {
            if !config_dir.is_dir() {
                create_dir_all(config_dir)?;
            }
        }

        if !config_path.is_file() {
            let mut file = File::create(config_path)?;
            file.write_all(&toml)?;
        }

        Ok(())
    }
    pub fn get_log_level(&self) -> log::Level {
        if self.log_level.to_lowercase() == "trace" {
            log::Level::Trace
        } else if self.log_level.to_lowercase() == "debug" {
            log::Level::Debug
        } else if self.log_level.to_lowercase() == "info" {
            log::Level::Info
        } else if self.log_level.to_lowercase() == "warn" {
            log::Level::Warn
        } else if self.log_level.to_lowercase() == "error" {
            log::Level::Error
        } else {
            log::Level::Info // default
        }
    }

    pub async fn get_server_address(&self) -> Result<SocketAddr, Box<dyn Error>> {
        match Ipv4Addr::from_str(&self.server_address) {
            Ok(ip_address) => Ok(SocketAddr::new(IpAddr::V4(ip_address), self.server_port)),
            Err(_e) => {
                // Let's try to look up hostname
                let host = format!("{}:{}", self.server_address, self.server_port);
                let mut addr_iter = lookup_host(host).await?;
                match addr_iter.next() {
                    Some(socket_address) => Ok(socket_address),
                    None => Err(format!(
                        "Could not resolve server address: {}",
                        self.server_address
                    )
                    .into()),
                }
            }
        }
    }
}