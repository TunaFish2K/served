use std::{env, io, path::PathBuf};

#[derive(Debug, Clone)]
pub struct ServedPaths {
    pub config_home: PathBuf,
    pub runtime_dir: PathBuf,
    pub state_home: PathBuf,
}

impl ServedPaths {
    pub fn from_environment() -> io::Result<Self> {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        if !home.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HOME must be an absolute path",
            ));
        }
        Ok(Self::from_home(home))
    }

    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let config_home = home.join(".config");
        let state_home = home.join(".local").join("state");
        let runtime_dir = state_home.join("served").join("runtime");
        Self {
            config_home,
            runtime_dir,
            state_home,
        }
    }

    pub fn registry_dir(&self) -> PathBuf {
        self.config_home.join("served").join("enabled")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("served.sock")
    }

    pub fn manager_generation(&self) -> PathBuf {
        self.runtime_dir.join("manager.generation")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state_home.join("served").join("logs")
    }

    pub fn runners_dir(&self) -> PathBuf {
        self.runtime_dir.join("runners")
    }

    pub fn runner_dir(&self, name: &str) -> PathBuf {
        self.runners_dir().join(name)
    }

    pub fn runner_socket(&self, name: &str) -> PathBuf {
        self.runner_dir(name).join("runner.sock")
    }

    pub fn runner_metadata(&self, name: &str) -> PathBuf {
        self.runner_dir(name).join("runner.json")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::ServedPaths;

    #[test]
    fn fixed_paths_are_derived_from_home() {
        let paths = ServedPaths::from_home("/tmp/served-home");

        assert_eq!(paths.config_home, Path::new("/tmp/served-home/.config"));
        assert_eq!(paths.state_home, Path::new("/tmp/served-home/.local/state"));
        assert_eq!(
            paths.runtime_dir,
            Path::new("/tmp/served-home/.local/state/served/runtime")
        );
        assert_eq!(
            paths.socket_path(),
            Path::new("/tmp/served-home/.local/state/served/runtime/served.sock")
        );
        assert_eq!(
            paths.manager_generation(),
            Path::new("/tmp/served-home/.local/state/served/runtime/manager.generation")
        );
        assert_eq!(
            paths.logs_dir(),
            Path::new("/tmp/served-home/.local/state/served/logs")
        );
        assert_eq!(
            paths.runners_dir(),
            Path::new("/tmp/served-home/.local/state/served/runtime/runners")
        );
        assert_eq!(
            paths.runner_socket("api"),
            Path::new("/tmp/served-home/.local/state/served/runtime/runners/api/runner.sock")
        );
    }
}
