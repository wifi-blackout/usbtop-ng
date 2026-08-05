use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_DIR_NAME: &str = ".usbtop-ng";
pub const PREFERENCES_FILE_NAME: &str = "preferences.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// Load the Linux usbmon kernel module automatically when it is missing.
    pub auto_load_usbmon: bool,
    /// Unload usbmon automatically on exit when usbtop-ng loaded it for this run.
    pub unload_usbmon_on_exit: bool,
}

impl Preferences {
    pub fn load_or_create_default() -> Result<Self> {
        let path = preferences_path()?;
        if let Some(parent) = path.parent() {
            ensure_private_config_dir(parent)?;
        }
        load_or_create_default_at(&path)
    }
}

pub fn preferences_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set; cannot locate ~/.usbtop-ng")?;
    Ok(PathBuf::from(home)
        .join(CONFIG_DIR_NAME)
        .join(PREFERENCES_FILE_NAME))
}

pub fn load_or_create_default_at(path: &Path) -> Result<Preferences> {
    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read preferences from {}", path.display()))?;
        return toml::from_str(&content)
            .with_context(|| format!("failed to parse preferences in {}", path.display()));
    }

    let prefs = Preferences::default();
    write_preferences_at(path, &prefs)?;
    Ok(prefs)
}

pub fn write_preferences_at(path: &Path, prefs: &Preferences) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create preferences directory {}",
                parent.display()
            )
        })?;
    }

    let content = toml::to_string_pretty(prefs).context("failed to serialize preferences")?;
    fs::write(path, content)
        .with_context(|| format!("failed to write preferences to {}", path.display()))?;
    Ok(())
}

/// Create the default config directory with private (0700) permissions.
/// Only chmods when this call creates the directory; an existing directory
/// (or a user-supplied custom path) is never re-chmodded.
pub fn ensure_private_config_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    set_private_dir_permissions(dir)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_default_preferences_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");

        let prefs = load_or_create_default_at(&path).unwrap();

        assert_eq!(prefs, Preferences::default());
        let written = fs::read_to_string(path).unwrap();
        assert!(written.contains("auto_load_usbmon = false"));
        assert!(written.contains("unload_usbmon_on_exit = false"));
    }

    #[test]
    fn loads_existing_preferences_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "auto_load_usbmon = true\nunload_usbmon_on_exit = true\n",
        )
        .unwrap();

        let prefs = load_or_create_default_at(&path).unwrap();

        assert!(prefs.auto_load_usbmon);
        assert!(prefs.unload_usbmon_on_exit);
    }

    #[cfg(unix)]
    #[test]
    fn custom_path_write_does_not_change_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("custom");
        fs::create_dir_all(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        let path = parent.join("prefs.toml");
        load_or_create_default_at(&path).unwrap();

        let mode = fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "custom parent dir permissions must be untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_config_dir_creates_with_0700() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".usbtop-ng");

        ensure_private_config_dir(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_config_dir_leaves_existing_dir_permissions_alone() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".usbtop-ng");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_config_dir(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "existing dir must not be re-chmodded");
    }
}
