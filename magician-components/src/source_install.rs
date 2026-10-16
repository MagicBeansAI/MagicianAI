//! The source-checkout installation contract shared by terminal and Desktop setup.
//! Execution stays in the existing Makefile and the source-install script.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Prepare,
    Start,
    Verify,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Start => "start",
            Self::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceInstall {
    pub checkout: PathBuf,
    pub data_root: PathBuf,
    pub prefix: PathBuf,
}

impl SourceInstall {
    pub fn new(checkout: &Path, data_root: &Path, prefix: &Path) -> Result<Self, String> {
        for path in [checkout, data_root, prefix] {
            if !path.is_absolute() {
                return Err(
                    "Source installation needs absolute checkout, data and prefix paths".into(),
                );
            }
        }
        if data_root.starts_with(prefix) || prefix.starts_with(data_root) {
            return Err("Runtime data and installed software need separate directories".into());
        }
        Ok(Self {
            checkout: checkout.into(),
            data_root: data_root.into(),
            prefix: prefix.into(),
        })
    }

    pub fn script(&self) -> PathBuf {
        self.checkout.join("scripts/install-source-checkout.sh")
    }

    pub fn arguments(
        &self,
        stage: Stage,
        dry_run: bool,
        desktop_running: bool,
    ) -> Vec<std::ffi::OsString> {
        let mut args = vec![
            self.script().into_os_string(),
            "--phase".into(),
            stage.as_str().into(),
            "--data-dir".into(),
            self.data_root.as_os_str().into(),
            "--prefix".into(),
            self.prefix.as_os_str().into(),
        ];
        if dry_run {
            args.push("--dry-run".into());
        }
        if desktop_running {
            args.push("--desktop-running".into());
        }
        args
    }
}

pub fn is_checkout(path: &Path) -> bool {
    path.join("magician-bin/Cargo.toml").is_file()
        && path.join("Makefile").is_file()
        && path.join("scripts/install-source-checkout.sh").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_spaces_stay_separate_arguments_and_dry_run_is_explicit() {
        let plan = SourceInstall::new(
            Path::new("/source checkout"),
            Path::new("/my notes"),
            Path::new("/software"),
        )
        .unwrap();
        let args = plan.arguments(Stage::Prepare, true, true);
        assert_eq!(
            args[0],
            "/source checkout/scripts/install-source-checkout.sh"
        );
        assert_eq!(args[4], "/my notes");
        assert!(args.iter().any(|arg| arg == "--dry-run"));
        assert!(args.iter().any(|arg| arg == "--desktop-running"));
    }

    #[test]
    fn rejects_relative_paths_and_a_prefix_that_is_the_data_root() {
        assert!(SourceInstall::new(
            Path::new("relative"),
            Path::new("/notes"),
            Path::new("/software")
        )
        .is_err());
        assert!(SourceInstall::new(
            Path::new("/checkout"),
            Path::new("/notes"),
            Path::new("/notes")
        )
        .is_err());
    }
}
