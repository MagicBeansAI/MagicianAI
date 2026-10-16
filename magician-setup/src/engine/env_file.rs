//! Writing a value into the operator's env file.
//!
//! Separate from the prompt that collects it, because the two fail differently:
//! a prompt can be cancelled and that is ordinary, while a write that half
//! succeeds leaves a file somebody has to repair by hand.
//!
//! Three rules, all of them about not destroying something already there:
//!
//! - **Never overwrite a key that is already set.** Re-running the wizard after
//!   pasting a key must not offer to replace it with a typo.
//! - **Preserve existing values and comments.** Empty assignments for the key
//!   being configured are replaced, because dotenv uses the first assignment.
//! - **0600, always.** A file holding a provider token should not be readable
//!   by every process on the machine, and the wizard is the one creating it.

use std::io::Write;
use std::path::{Path, PathBuf};

pub struct EnvFile {
    path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Wrote {
    /// Appended. The value is now in the file.
    Added,
    /// Already set, and left exactly as it was.
    AlreadySet,
}

impl EnvFile {
    pub fn at(data_root: &Path) -> Self {
        EnvFile {
            path: data_root.join(".env"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the variable already has a non-empty value.
    ///
    /// Commented lines do not count: `# OPENAI_API_KEY=sk-...` in the shipped
    /// example is a suggestion, not a setting, and treating it as set would
    /// make the wizard skip the one key it most needs to ask for.
    pub fn is_set(&self, variable: &str) -> bool {
        self.is_set_one(variable)
            || Self {
                path: self.path.with_file_name(".env.development"),
            }
            .is_set_one(variable)
    }

    fn is_set_one(&self, variable: &str) -> bool {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return false;
        };
        text.lines().any(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return false;
            }
            match line.split_once('=') {
                Some((name, value)) => {
                    name.trim() == variable && !value.trim().trim_matches(['"', '\'']).is_empty()
                },
                None => false,
            }
        })
    }

    /// Append `variable=value`, unless it is already set.
    pub fn set(&self, variable: &str, value: &str) -> std::io::Result<Wrote> {
        if variable.is_empty()
            || !variable
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || value.contains(['\n', '\r', '\0'])
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid environment assignment",
            ));
        }
        if self.is_set(variable) {
            return Ok(Wrote::AlreadySet);
        }
        let wrote = self.set_one(variable, value)?;
        let development = Self {
            path: self.path.with_file_name(".env.development"),
        };
        development.set_one(variable, value)?;
        Ok(wrote)
    }

    fn set_one(&self, variable: &str, value: &str) -> std::io::Result<Wrote> {
        if self.is_set_one(variable) {
            return Ok(Wrote::AlreadySet);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let existing = std::fs::read_to_string(&self.path).unwrap_or_default();
        let value = if value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_+-./:=@".contains(c))
        {
            value.to_owned()
        } else {
            format!(
                "\"{}\"",
                value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
            )
        };
        let has_empty_assignment = existing.lines().any(|line| {
            !line.trim_start().starts_with('#')
                && line
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim() == variable)
        });
        if has_empty_assignment {
            // Keep every unrelated line verbatim; an earlier empty assignment
            // would otherwise hide the appended credential from dotenv.
            let mut updated = String::new();
            for line in existing.split_inclusive('\n') {
                if !line.trim_start().starts_with('#')
                    && line
                        .split_once('=')
                        .is_some_and(|(key, _)| key.trim() == variable)
                {
                    updated.push_str(&format!("{variable}={value}\n"));
                } else {
                    updated.push_str(line);
                }
            }
            let staging = self
                .path
                .with_extension(format!("setup-{}", std::process::id()));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging)?;
            harden(&staging)?;
            let result = (|| {
                file.write_all(updated.as_bytes())?;
                file.flush()?;
                std::fs::rename(&staging, &self.path)
            })();
            let _ = std::fs::remove_file(staging);
            result?;
            return Ok(Wrote::Added);
        }
        // A file that does not end in a newline would otherwise get the new key
        // welded onto its last line.
        let needs_newline = !existing.is_empty() && !existing.ends_with('\n');

        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        harden(&self.path)?;
        if needs_newline {
            writeln!(file)?;
        }
        writeln!(file, "{variable}={value}")?;
        file.flush()?;
        harden(&self.path)?;
        Ok(Wrote::Added)
    }
}

/// Owner read/write only. Best effort off Unix, where the concept differs.
#[cfg(unix)]
fn harden(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn harden(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
