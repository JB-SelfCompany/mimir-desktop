use directories::ProjectDirs;
use std::path::{Path, PathBuf};

pub struct DataPaths {
    pub root: PathBuf,
    pub core_dir: PathBuf,
    pub files_dir: PathBuf,
    pub settings_file: PathBuf,
}

impl DataPaths {
    pub fn under(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            core_dir: root.join("mimir"),
            files_dir: root.join("files"),
            settings_file: root.join("settings.json"),
        }
    }

    /// `MIMIR_DATA_DIR`: a separate data folder (second instance, smoke runs). Empty counts as unset.
    fn from_override(dir: Option<std::ffi::OsString>) -> Option<Self> {
        dir.filter(|d| !d.is_empty()).map(|d| Self::under(Path::new(&d)))
    }

    pub fn default_for_user() -> Self {
        if let Some(p) = Self::from_override(std::env::var_os("MIMIR_DATA_DIR")) {
            return p;
        }
        // ponytail: no fallback if the OS has no home dir; panics instead.
        let dirs = ProjectDirs::from("im", "mimir", "Mimir").expect("no home directory");
        Self::under(dirs.data_dir())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_override() {
        assert_eq!(DataPaths::from_override(Some("D:/m".into())).map(|p| p.root), Some(PathBuf::from("D:/m")));
        assert!(DataPaths::from_override(Some("".into())).is_none(), "empty = unset, not the current directory");
        assert!(DataPaths::from_override(None).is_none());
    }

    #[test]
    fn paths_layout() {
        let p = DataPaths::under(Path::new("X"));
        assert_eq!(p.core_dir, Path::new("X").join("mimir"));
        assert_eq!(p.files_dir, Path::new("X").join("files"));
        assert_eq!(p.settings_file, Path::new("X").join("settings.json"));
    }
}
