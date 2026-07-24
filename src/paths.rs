use std::{env, io, path::PathBuf};

#[derive(Debug, Clone)]
pub struct ServedPaths {
    pub config_home: PathBuf,
    pub runtime_dir: PathBuf,
    pub state_home: PathBuf,
}

impl ServedPaths {
    pub fn from_environment() -> io::Result<Self> {
        let config_home = match env::var_os("XDG_CONFIG_HOME") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?,
        };
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
        let state_home = match env::var_os("XDG_STATE_HOME") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local").join("state"))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?,
        };
        Ok(Self {
            config_home,
            runtime_dir,
            state_home,
        })
    }

    pub fn registry_dir(&self) -> PathBuf {
        self.config_home.join("served").join("enabled")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("served.sock")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state_home.join("served").join("logs")
    }
}
