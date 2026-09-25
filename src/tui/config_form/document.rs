use super::source;
use crate::config::ServiceConfig;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 1024 * 1024;

pub(super) struct Document {
    pub path: PathBuf,
    target: PathBuf,
    original: String,
    pub saved_text: String,
    pub config: ServiceConfig,
    crlf: bool,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self> {
        let target = fs::canonicalize(path).context("resolve configuration path")?;
        let meta = fs::metadata(&target)?;
        if !meta.is_file() {
            bail!("configuration must be a regular file");
        }
        if meta.len() > MAX_BYTES {
            bail!("built-in editor supports files up to 1 MiB; use --editor for larger files");
        }
        let original = fs::read_to_string(&target).context("read configuration as UTF-8")?;
        let crlf = original.contains("\r\n") && !original.replace("\r\n", "").contains('\n');
        let saved_text = original.replace("\r\n", "\n");
        let config = decode(&saved_text)?;
        Ok(Self {
            path: path.to_owned(),
            target,
            original,
            saved_text,
            config,
            crlf,
        })
    }

    pub fn validate(text: &str) -> Result<()> {
        let config: ServiceConfig = json5::from_str(text).context("Invalid JSON5 configuration")?;
        config.validate().context("Invalid service configuration")?;
        Ok(())
    }

    fn check_unchanged(&self) -> Result<()> {
        if fs::canonicalize(&self.path).ok().as_ref() != Some(&self.target)
            || fs::read_to_string(&self.target).ok().as_ref() != Some(&self.original)
        {
            bail!(
                "File changed outside this editor. Your changes were not written. Use Ctrl+R to reload, or return to editing and copy your changes before reloading."
            );
        }
        Ok(())
    }

    pub fn save_config(&mut self, config: &ServiceConfig) -> Result<()> {
        config.validate()?;
        let before = serde_json::to_value(&self.config)?;
        let after = serde_json::to_value(config)?;
        let mut text = self.saved_text.clone();
        for key in [
            "name",
            "command",
            "cwd",
            "tty",
            "syncRowsCols",
            "restart",
            "persist_logs",
            "log_max_bytes",
            "log_max_files",
        ] {
            if before.get(key) != after.get(key) {
                text = source::set(
                    &text,
                    None,
                    key,
                    Some(after.get(key).unwrap_or(&Value::Null)),
                )?;
            }
        }
        if config.env != self.config.env {
            if source::parse(&text)?.get("env").is_none() {
                text = source::set(&text, None, "env", Some(&serde_json::json!({})))?;
            }
            for key in self
                .config
                .env
                .keys()
                .filter(|key| !config.env.contains_key(*key))
            {
                text = source::set(&text, Some("env"), key, None)?;
            }
            for (key, value) in &config.env {
                if self.config.env.get(key) != Some(value) {
                    text =
                        source::set(&text, Some("env"), key, Some(&Value::String(value.clone())))?;
                }
            }
        }
        let generated = decode(&text)?;
        if generated != *config {
            bail!("generated configuration does not match the form; file was not changed");
        }
        self.save(&text)?;
        self.config = config.clone();
        Ok(())
    }

    pub fn save(&mut self, text: &str) -> Result<()> {
        if text.len() as u64 > MAX_BYTES {
            bail!("configuration exceeds 1 MiB; use --editor for larger files");
        }
        Self::validate(text)?;
        self.check_unchanged()?;
        if text == self.saved_text {
            return Ok(());
        }
        let metadata = fs::metadata(&self.target)?;
        if metadata.permissions().mode() & 0o222 == 0 {
            bail!("configuration is read-only; permissions were not changed");
        }
        if metadata.nlink() > 1 {
            bail!("configuration has multiple hard links; use --editor to preserve them");
        }
        let parent = self
            .target
            .parent()
            .context("configuration has no parent directory")?;
        let temporary = parent.join(format!(
            ".served-edit-{}-{:016x}.tmp",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .context("create temporary configuration")?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(temporary.clone());
        let output = if self.crlf {
            text.replace('\n', "\r\n")
        } else {
            text.to_owned()
        };
        file.write_all(output.as_bytes())
            .context("write configuration")?;
        file.set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))?;
        file.sync_all().context("sync configuration")?;
        self.check_unchanged()?;
        fs::rename(&temporary, &self.target).context("replace configuration")?;
        self.original = output;
        self.saved_text = text.to_owned();
        Ok(())
    }
}

fn decode(text: &str) -> Result<ServiceConfig> {
    let mut value = source::parse(text)?;
    let object = value
        .as_object_mut()
        .context("configuration must be an object")?;
    for key in ["name", "command"] {
        object
            .entry(key)
            .or_insert_with(|| Value::String(String::new()));
    }
    serde_json::from_value(value).context(
        "Unsupported configuration field or type; repair using served edit --editor COMMAND",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    const TEXT: &str = "//keep\n{name:'api', command:'true', env:{ A:'1', /*B*/ B:'中', },}\n";
    #[test]
    fn form_save_changes_only_fields_and_env_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, TEXT).unwrap();
        let mut doc = Document::open(&path).unwrap();
        let mut config = doc.config.clone();
        config.command = "echo '中文'\ntrue".into();
        config.env.remove("A");
        config.env.insert("B".into(), "new".into());
        config.env.insert("C".into(), "3".into());
        doc.save_config(&config).unwrap();
        let output = fs::read_to_string(&path).unwrap();
        assert!(output.starts_with("//keep\n{name:'api',"));
        assert!(output.contains("/*B*/"));
        assert_eq!(decode(&output).unwrap(), config);
        let inode = fs::metadata(&path).unwrap().ino();
        doc.save_config(&config).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    }
    #[test]
    fn missing_fields_and_conflicting_saves_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, "{}").unwrap();
        let mut doc = Document::open(&path).unwrap();
        let mut config = doc.config.clone();
        assert!(doc.save_config(&config).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        config.name = "api".into();
        config.command = "true".into();
        doc.save_config(&config).unwrap();
        fs::write(&path, "//external\n{}").unwrap();
        config.command = "false".into();
        assert!(doc.save_config(&config).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "//external\n{}");
    }
    #[test]
    fn crlf_permissions_symlink_and_optional_defaults_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let link = dir.path().join("link");
        fs::write(&path, TEXT.replace('\n', "\r\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&path, &link).unwrap();
        let mut doc = Document::open(&link).unwrap();
        let mut config = doc.config.clone();
        config.cwd = Some("../work".into());
        doc.save_config(&config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\r\n"));
        assert!(!text.contains("log_max_bytes"));
        assert!(link.is_symlink());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        config.cwd = None;
        doc.save_config(&config).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("null"));
    }
}
