//! Descriptor-backed physical owner for the small filesystem/table Apps slice.
//!
//! App arguments never reach `std::fs` or DuckDB as ambient host paths. The
//! owner opens the reviewed capability directory once, walks every component
//! with `openat(O_NOFOLLOW)`, retains the resulting descriptors through I/O,
//! and re-walks the exact relative path at the final fence. Writes publish a
//! fully fsynced same-directory temporary file with an atomic no-replace or
//! exchange operation. Platforms without those primitives stay unavailable.

use std::path::{Component, Path};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::models::AppDigest;
use super::{models::AppReference, tool_disclosure::AttestedAppToolTarget};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::magician_v2::execution::actions::FileAction;
use crate::magician_v2::execution::actions::{ActionResult, DuckDbAction, ExecutableAction};

pub const APP_BOUND_FILE_CONTENT_CEILING: u64 = 1024 * 1024;
pub const APP_BOUND_FILE_INPUT_CEILING: u64 = 6 * 1024 * 1024 + 64 * 1024;
pub const APP_BOUND_FILE_RESULT_CEILING: u64 = 6 * 1024 * 1024 + 64 * 1024;
pub const APP_BOUND_TABLE_SOURCE_CEILING: u64 = 16 * 1024 * 1024;
pub const APP_BOUND_TABLE_INPUT_CEILING: u64 = 32 * 1024;
pub const APP_BOUND_TABLE_RESULT_CEILING: u64 = 8 * 1024 * 1024 + 64 * 1024;
pub const APP_BOUND_PATH_OWNER_SUPPORTED: bool =
    cfg!(any(target_os = "macos", target_os = "linux"));
const MAX_RELATIVE_PATH_BYTES: usize = 1024;
const MAX_PATH_COMPONENTS: usize = 64;

#[derive(Debug, Error)]
pub enum AppBoundPathError {
    #[error("bound path operation is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("bound path is not an exact normalized relative path")]
    InvalidRelativePath,
    #[error("bound path operation is outside the reviewed action subset")]
    UnsupportedAction,
    #[error("bound path operation exceeds its reviewed byte ceiling")]
    ByteCeiling,
    #[error("bound path target is a link, hardlink, special file, or mount escape")]
    UnsafeTarget,
    #[error("bound path target changed after review")]
    IdentityChanged,
    #[error("bound path I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("bound path identity could not be encoded")]
    IdentityEncoding,
}

/// A move-only descriptor owner. It deliberately implements neither Clone,
/// Debug nor Serde so the reviewed authority cannot be replayed as data.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct PreparedAppBoundPath {
    directory: CapabilityDirectory,
    operation: PreparedOperation,
    binding_digest: AppDigest,
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub struct PreparedAppBoundPath;

#[cfg(any(target_os = "macos", target_os = "linux"))]
enum PreparedOperation {
    Read {
        relative_path: String,
        file: File,
        identity: FileIdentity,
    },
    Write {
        relative_path: String,
        parent: File,
        parent_identity: FileIdentity,
        name: CString,
        expected: WriteTarget,
        content: Vec<u8>,
    },
    Table {
        relative_path: String,
        file: File,
        identity: FileIdentity,
        operation: TableOperation,
        format: TableFormat,
        limit: u32,
        output_format: String,
        timeout_secs: Option<u64>,
    },
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum TableOperation {
    Preview,
    Describe,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum TableFormat {
    Csv,
    Json,
    Parquet,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
enum WriteTarget {
    CreateNew,
    Replace { file: File, identity: FileIdentity },
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct CapabilityDirectory {
    root: File,
    identity: FileIdentity,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl PreparedAppBoundPath {
    pub fn prepare(
        action: &ExecutableAction,
        parameters: &std::collections::HashMap<String, Value>,
        workdir: &Path,
    ) -> Result<Self, AppBoundPathError> {
        #[cfg(not(unix))]
        {
            let _ = (action, parameters, workdir);
            return Err(AppBoundPathError::UnsupportedPlatform);
        }
        #[cfg(unix)]
        {
            let directory = CapabilityDirectory::open(workdir)?;
            let operation = match action {
                ExecutableAction::File(FileAction::Read { path, encoding }) => {
                    if encoding.as_deref().is_some_and(|value| {
                        !matches!(value.trim().to_ascii_lowercase().as_str(), "utf8" | "utf-8")
                    }) {
                        return Err(AppBoundPathError::UnsupportedAction);
                    }
                    let relative_path = normalize_app_relative_path(path)?;
                    let (file, identity) = directory.open_regular(&relative_path)?;
                    if identity.size > APP_BOUND_FILE_CONTENT_CEILING {
                        return Err(AppBoundPathError::ByteCeiling);
                    }
                    PreparedOperation::Read {
                        relative_path,
                        file,
                        identity,
                    }
                },
                ExecutableAction::File(FileAction::Write { path, content, .. }) => {
                    if u64::try_from(content.len())
                        .ok()
                        .is_none_or(|size| size > APP_BOUND_FILE_CONTENT_CEILING)
                    {
                        return Err(AppBoundPathError::ByteCeiling);
                    }
                    let relative_path = normalize_app_relative_path(path)?;
                    let (parent, parent_identity, name) = directory.open_parent(&relative_path)?;
                    let expected = match stat_at(parent.as_raw_fd(), &name)? {
                        None => WriteTarget::CreateNew,
                        Some(identity) => {
                            require_regular_file(identity, directory.identity.device)?;
                            let file = open_file_at(parent.as_raw_fd(), &name, libc::O_RDONLY)?;
                            let opened = file_identity(file.as_raw_fd())?;
                            if opened != identity {
                                return Err(AppBoundPathError::IdentityChanged);
                            }
                            WriteTarget::Replace { file, identity }
                        },
                    };
                    PreparedOperation::Write {
                        relative_path,
                        parent,
                        parent_identity,
                        name,
                        expected,
                        content: content.as_bytes().to_vec(),
                    }
                },
                ExecutableAction::DuckDb(action) => {
                    if parameters
                        .get("database")
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty())
                        || parameters.get("is_table").and_then(Value::as_bool) == Some(true)
                    {
                        return Err(AppBoundPathError::UnsupportedAction);
                    }
                    let selected = parameters
                        .get("__action_name")
                        .and_then(Value::as_str)
                        .ok_or(AppBoundPathError::UnsupportedAction)?;
                    let operation = match selected {
                        "preview" => TableOperation::Preview,
                        "describe" => TableOperation::Describe,
                        _ => return Err(AppBoundPathError::UnsupportedAction),
                    };
                    let source = parameters
                        .get("source")
                        .and_then(Value::as_str)
                        .ok_or(AppBoundPathError::InvalidRelativePath)?;
                    let relative_path = normalize_app_relative_path(Path::new(source))?;
                    let format = table_format(&relative_path)?;
                    let (file, identity) = directory.open_regular(&relative_path)?;
                    if identity.size > APP_BOUND_TABLE_SOURCE_CEILING {
                        return Err(AppBoundPathError::ByteCeiling);
                    }
                    let limit = match parameters.get("limit") {
                        None => 10,
                        Some(value) => value
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .filter(|value| (1..=1000).contains(value))
                            .ok_or(AppBoundPathError::UnsupportedAction)?,
                    };
                    if matches!(operation, TableOperation::Describe)
                        && parameters.contains_key("limit")
                    {
                        return Err(AppBoundPathError::UnsupportedAction);
                    }
                    let output_format = action.output_format.trim().to_ascii_lowercase();
                    if output_format != "json" {
                        return Err(AppBoundPathError::UnsupportedAction);
                    }
                    PreparedOperation::Table {
                        relative_path,
                        file,
                        identity,
                        operation,
                        format,
                        limit,
                        output_format,
                        timeout_secs: action.timeout_secs,
                    }
                },
                _ => return Err(AppBoundPathError::UnsupportedAction),
            };
            let binding_digest = binding_digest(&directory, &operation)?;
            Ok(Self {
                directory,
                operation,
                binding_digest,
            })
        }
    }

    pub fn attest(&self, tool_ref: AppReference) -> Option<AttestedAppToolTarget> {
        self.verify_current().ok()?;
        let suffix = self.binding_digest.as_str().strip_prefix("blake3:")?;
        let runtime_ref =
            AppReference::parse(format!("runtime:compiled:app-bound-path:{suffix}")).ok()?;
        Some(AttestedAppToolTarget::from_trusted_local_dispatcher(
            tool_ref,
            runtime_ref,
        ))
    }

    pub fn matches_action(&self, action: &ExecutableAction) -> bool {
        matches!(
            (&self.operation, action),
            (
                PreparedOperation::Read { .. },
                ExecutableAction::File(FileAction::Read { .. })
            ) | (
                PreparedOperation::Write { .. },
                ExecutableAction::File(FileAction::Write { .. })
            ) | (PreparedOperation::Table { .. }, ExecutableAction::DuckDb(_))
        )
    }

    pub fn is_table(&self) -> bool {
        matches!(self.operation, PreparedOperation::Table { .. })
    }

    pub fn bound_table_action(&self) -> Result<DuckDbAction, AppBoundPathError> {
        let PreparedOperation::Table {
            file,
            operation,
            format,
            limit,
            output_format,
            timeout_secs,
            ..
        } = &self.operation
        else {
            return Err(AppBoundPathError::UnsupportedAction);
        };
        self.verify_current()?;
        let descriptor = descriptor_path(file.as_raw_fd())?;
        let escaped = descriptor.replace('\'', "''");
        let reader = match format {
            TableFormat::Csv => format!("read_csv_auto('{escaped}')"),
            TableFormat::Json => format!("read_json_auto('{escaped}')"),
            TableFormat::Parquet => format!("read_parquet('{escaped}')"),
        };
        let sql = match operation {
            TableOperation::Preview => format!("SELECT * FROM {reader} LIMIT {limit}"),
            TableOperation::Describe => format!("DESCRIBE SELECT * FROM {reader}"),
        };
        Ok(DuckDbAction {
            sql,
            database: None,
            output_format: output_format.clone(),
            timeout_secs: *timeout_secs,
        })
    }

    pub fn execute_file(mut self) -> Result<ActionResult, AppBoundPathError> {
        self.verify_current()?;
        match &mut self.operation {
            PreparedOperation::Read { file, identity, .. } => {
                file.seek(SeekFrom::Start(0))?;
                let mut bounded = file.take(APP_BOUND_FILE_CONTENT_CEILING + 1);
                let mut bytes = Vec::with_capacity(
                    usize::try_from(identity.size)
                        .unwrap_or_default()
                        .min(usize::try_from(APP_BOUND_FILE_CONTENT_CEILING).unwrap_or(0)),
                );
                bounded.read_to_end(&mut bytes)?;
                if u64::try_from(bytes.len())
                    .ok()
                    .is_none_or(|size| size > APP_BOUND_FILE_CONTENT_CEILING)
                    || file_identity(bounded.get_ref().as_raw_fd())? != *identity
                {
                    return Err(AppBoundPathError::IdentityChanged);
                }
                let content =
                    String::from_utf8(bytes).map_err(|_| AppBoundPathError::UnsupportedAction)?;
                Ok(ActionResult::text(content))
            },
            PreparedOperation::Write {
                parent,
                name,
                expected,
                content,
                ..
            } => {
                publish_atomic(parent, name, expected, content)?;
                Ok(ActionResult::success())
            },
            PreparedOperation::Table { .. } => Err(AppBoundPathError::UnsupportedAction),
        }
    }

    pub fn verify_current(&self) -> Result<(), AppBoundPathError> {
        if file_identity(self.directory.root.as_raw_fd())? != self.directory.identity {
            return Err(AppBoundPathError::IdentityChanged);
        }
        match &self.operation {
            PreparedOperation::Read {
                relative_path,
                file,
                identity,
            }
            | PreparedOperation::Table {
                relative_path,
                file,
                identity,
                ..
            } => {
                if file_identity(file.as_raw_fd())? != *identity {
                    return Err(AppBoundPathError::IdentityChanged);
                }
                let (current, current_identity) = self.directory.open_regular(relative_path)?;
                if file_identity(current.as_raw_fd())? != current_identity
                    || current_identity != *identity
                {
                    return Err(AppBoundPathError::IdentityChanged);
                }
            },
            PreparedOperation::Write {
                relative_path,
                parent,
                parent_identity,
                name,
                expected,
                ..
            } => {
                if file_identity(parent.as_raw_fd())? != *parent_identity {
                    return Err(AppBoundPathError::IdentityChanged);
                }
                let (current_parent, current_parent_identity, current_name) =
                    self.directory.open_parent(relative_path)?;
                if current_parent_identity != *parent_identity
                    || current_name.as_c_str() != name.as_c_str()
                {
                    return Err(AppBoundPathError::IdentityChanged);
                }
                let current = stat_at(current_parent.as_raw_fd(), &current_name)?;
                match (expected, current) {
                    (WriteTarget::CreateNew, None) => {},
                    (WriteTarget::Replace { file, identity }, Some(current))
                        if file_identity(file.as_raw_fd())? == *identity
                            && current == *identity => {},
                    _ => return Err(AppBoundPathError::IdentityChanged),
                }
            },
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl PreparedAppBoundPath {
    pub fn prepare(
        action: &ExecutableAction,
        parameters: &std::collections::HashMap<String, Value>,
        workdir: &Path,
    ) -> Result<Self, AppBoundPathError> {
        let _ = (action, parameters, workdir);
        Err(AppBoundPathError::UnsupportedPlatform)
    }

    pub fn attest(&self, tool_ref: AppReference) -> Option<AttestedAppToolTarget> {
        let _ = tool_ref;
        None
    }

    pub fn matches_action(&self, action: &ExecutableAction) -> bool {
        let _ = action;
        false
    }

    pub fn is_table(&self) -> bool {
        false
    }

    pub fn bound_table_action(&self) -> Result<DuckDbAction, AppBoundPathError> {
        Err(AppBoundPathError::UnsupportedPlatform)
    }

    pub fn execute_file(self) -> Result<ActionResult, AppBoundPathError> {
        Err(AppBoundPathError::UnsupportedPlatform)
    }

    pub fn verify_current(&self) -> Result<(), AppBoundPathError> {
        Err(AppBoundPathError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl CapabilityDirectory {
    fn open(root: &Path) -> Result<Self, AppBoundPathError> {
        let root = CString::new(root.as_os_str().as_encoded_bytes())
            .map_err(|_| AppBoundPathError::InvalidRelativePath)?;
        let fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let root = unsafe { File::from_raw_fd(fd) };
        let identity = file_identity(root.as_raw_fd())?;
        require_directory(identity)?;
        Ok(Self { root, identity })
    }

    fn open_parent(
        &self,
        relative: &str,
    ) -> Result<(File, FileIdentity, CString), AppBoundPathError> {
        let parts = relative_components(relative)?;
        let (name, parents) = parts
            .split_last()
            .ok_or(AppBoundPathError::InvalidRelativePath)?;
        let mut directory = self.root.try_clone()?;
        for component in parents {
            directory = open_directory_at(directory.as_raw_fd(), component, self.identity.device)?;
        }
        let identity = file_identity(directory.as_raw_fd())?;
        require_directory(identity)?;
        if identity.device != self.identity.device {
            return Err(AppBoundPathError::UnsafeTarget);
        }
        Ok((directory, identity, name.clone()))
    }

    fn open_regular(&self, relative: &str) -> Result<(File, FileIdentity), AppBoundPathError> {
        let (parent, _, name) = self.open_parent(relative)?;
        let file = open_file_at(parent.as_raw_fd(), &name, libc::O_RDONLY)?;
        let identity = file_identity(file.as_raw_fd())?;
        require_regular_file(identity, self.identity.device)?;
        let named =
            stat_at(parent.as_raw_fd(), &name)?.ok_or(AppBoundPathError::IdentityChanged)?;
        if named != identity {
            return Err(AppBoundPathError::IdentityChanged);
        }
        Ok((file, identity))
    }
}

pub(crate) fn normalize_app_relative_path(path: &Path) -> Result<String, AppBoundPathError> {
    let raw = path.as_os_str().as_encoded_bytes();
    if raw.is_empty()
        || raw.len() > MAX_RELATIVE_PATH_BYTES
        || !raw.is_ascii()
        || raw.iter().any(|byte| {
            !byte.is_ascii_alphanumeric() && !matches!(*byte, b'.' | b'_' | b'-' | b' ' | b'/')
        })
        || raw.contains(&0)
        || path.is_absolute()
    {
        return Err(AppBoundPathError::InvalidRelativePath);
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let bytes = part.as_encoded_bytes();
                if bytes.is_empty() || bytes == b"." || bytes == b".." {
                    return Err(AppBoundPathError::InvalidRelativePath);
                }
                parts.push(
                    std::str::from_utf8(bytes)
                        .map_err(|_| AppBoundPathError::InvalidRelativePath)?,
                );
            },
            _ => return Err(AppBoundPathError::InvalidRelativePath),
        }
    }
    if parts.is_empty() || parts.len() > MAX_PATH_COMPONENTS {
        return Err(AppBoundPathError::InvalidRelativePath);
    }
    Ok(parts.join("/"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn relative_components(relative: &str) -> Result<Vec<CString>, AppBoundPathError> {
    normalize_app_relative_path(Path::new(relative))?
        .split('/')
        .map(|part| CString::new(part).map_err(|_| AppBoundPathError::InvalidRelativePath))
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_directory_at(
    parent: RawFd,
    name: &CStr,
    root_device: u64,
) -> Result<File, AppBoundPathError> {
    let file = open_file_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY)?;
    let identity = file_identity(file.as_raw_fd())?;
    require_directory(identity)?;
    if identity.device != root_device {
        return Err(AppBoundPathError::UnsafeTarget);
    }
    Ok(file)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_file_at(parent: RawFd, name: &CStr, flags: i32) -> Result<File, AppBoundPathError> {
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stat_at(parent: RawFd, name: &CStr) -> Result<Option<FileIdentity>, AppBoundPathError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    let rc = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(Some(identity_from_stat(unsafe { stat.assume_init() })?));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        return Ok(None);
    }
    Err(error.into())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_identity(fd: RawFd) -> Result<FileIdentity, AppBoundPathError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    identity_from_stat(unsafe { stat.assume_init() })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn identity_from_stat(stat: libc::stat) -> Result<FileIdentity, AppBoundPathError> {
    let size = u64::try_from(stat.st_size).map_err(|_| AppBoundPathError::UnsafeTarget)?;
    let (modified_seconds, modified_nanoseconds) = (stat.st_mtime, stat.st_mtime_nsec);
    Ok(FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        mode: stat.st_mode as u32,
        links: stat.st_nlink as u64,
        size,
        modified_seconds,
        modified_nanoseconds,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn require_directory(identity: FileIdentity) -> Result<(), AppBoundPathError> {
    if identity.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFDIR) {
        return Err(AppBoundPathError::UnsafeTarget);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn require_regular_file(identity: FileIdentity, root_device: u64) -> Result<(), AppBoundPathError> {
    if identity.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
        || identity.device != root_device
        || identity.links != 1
    {
        return Err(AppBoundPathError::UnsafeTarget);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn table_format(relative: &str) -> Result<TableFormat, AppBoundPathError> {
    let extension = Path::new(relative)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or(AppBoundPathError::UnsupportedAction)?;
    match extension.as_str() {
        "csv" | "tsv" => Ok(TableFormat::Csv),
        "json" | "jsonl" | "ndjson" => Ok(TableFormat::Json),
        "parquet" => Ok(TableFormat::Parquet),
        _ => Err(AppBoundPathError::UnsupportedAction),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn descriptor_path(fd: RawFd) -> Result<String, AppBoundPathError> {
    #[cfg(target_os = "linux")]
    return Ok(format!("/proc/self/fd/{fd}"));
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    return Ok(format!("/dev/fd/{fd}"));
    #[allow(unreachable_code)]
    Err(AppBoundPathError::UnsupportedPlatform)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn binding_digest(
    directory: &CapabilityDirectory,
    operation: &PreparedOperation,
) -> Result<AppDigest, AppBoundPathError> {
    #[derive(Serialize)]
    struct Material<'a> {
        profile: &'static str,
        root: FileIdentity,
        operation: &'static str,
        relative_path: &'a str,
        target: Option<FileIdentity>,
        table_operation: Option<TableOperation>,
        table_format: Option<TableFormat>,
        byte_ceiling: u64,
        publish_semantics: Option<&'static str>,
    }
    let material = match operation {
        PreparedOperation::Read {
            relative_path,
            identity,
            ..
        } => Material {
            profile: "magician.app-bound-capability-directory.v1",
            root: directory.identity,
            operation: "read",
            relative_path,
            target: Some(*identity),
            table_operation: None,
            table_format: None,
            byte_ceiling: APP_BOUND_FILE_CONTENT_CEILING,
            publish_semantics: None,
        },
        PreparedOperation::Write {
            relative_path,
            expected,
            ..
        } => Material {
            profile: "magician.app-bound-capability-directory.v1",
            root: directory.identity,
            operation: "write",
            relative_path,
            target: match expected {
                WriteTarget::CreateNew => None,
                WriteTarget::Replace { identity, .. } => Some(*identity),
            },
            table_operation: None,
            table_format: None,
            byte_ceiling: APP_BOUND_FILE_CONTENT_CEILING,
            publish_semantics: Some(match expected {
                WriteTarget::CreateNew => "create_new_atomic",
                WriteTarget::Replace { .. } => "compare_exchange_atomic",
            }),
        },
        PreparedOperation::Table {
            relative_path,
            identity,
            operation,
            format,
            ..
        } => Material {
            profile: "magician.app-bound-capability-directory.v1",
            root: directory.identity,
            operation: "table_read",
            relative_path,
            target: Some(*identity),
            table_operation: Some(*operation),
            table_format: Some(*format),
            byte_ceiling: APP_BOUND_TABLE_SOURCE_CEILING,
            publish_semantics: None,
        },
    };
    AppDigest::blake3_canonical_json(
        &serde_json::to_value(material).map_err(|_| AppBoundPathError::IdentityEncoding)?,
    )
    .map_err(|_| AppBoundPathError::IdentityEncoding)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn publish_atomic(
    parent: &File,
    target_name: &CStr,
    expected: &WriteTarget,
    content: &[u8],
) -> Result<(), AppBoundPathError> {
    let temporary_name = CString::new(format!(".magician-app-{}.tmp", uuid::Uuid::new_v4()))
        .map_err(|_| AppBoundPathError::IdentityEncoding)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut temporary = unsafe { File::from_raw_fd(fd) };
    let mut cleanup_temporary_name = true;
    let result = (|| {
        write_complete(&mut temporary, content)?;
        temporary.sync_all()?;
        match expected {
            WriteTarget::CreateNew => {
                rename_no_replace(parent.as_raw_fd(), &temporary_name, target_name)?;
                cleanup_temporary_name = false;
            },
            WriteTarget::Replace { identity, .. } => {
                let current = stat_at(parent.as_raw_fd(), target_name)?
                    .ok_or(AppBoundPathError::IdentityChanged)?;
                if current != *identity {
                    return Err(AppBoundPathError::IdentityChanged);
                }
                rename_exchange(parent.as_raw_fd(), &temporary_name, target_name)?;
                // The temporary name now denotes the displaced target. Do not
                // unlink it on an uncertain recovery path.
                cleanup_temporary_name = false;
                let displaced = stat_at(parent.as_raw_fd(), &temporary_name)?
                    .ok_or(AppBoundPathError::IdentityChanged)?;
                if displaced != *identity {
                    if rename_exchange(parent.as_raw_fd(), &temporary_name, target_name).is_ok() {
                        cleanup_temporary_name = true;
                    }
                    return Err(AppBoundPathError::IdentityChanged);
                }
                unlink_at(parent.as_raw_fd(), &temporary_name)?;
            },
        }
        if unsafe { libc::fsync(parent.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    })();
    if result.is_err() && cleanup_temporary_name {
        let _ = unlink_at(parent.as_raw_fd(), &temporary_name);
    }
    result
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_complete(writer: &mut impl Write, content: &[u8]) -> std::io::Result<()> {
    writer.write_all(content)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unlink_at(parent: RawFd, name: &CStr) -> Result<(), AppBoundPathError> {
    if unsafe { libc::unlinkat(parent, name.as_ptr(), 0) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_no_replace(parent: RawFd, source: &CStr, target: &CStr) -> Result<(), AppBoundPathError> {
    if unsafe {
        libc::renameatx_np(
            parent,
            source.as_ptr(),
            parent,
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_exchange(parent: RawFd, source: &CStr, target: &CStr) -> Result<(), AppBoundPathError> {
    if unsafe {
        libc::renameatx_np(
            parent,
            source.as_ptr(),
            parent,
            target.as_ptr(),
            libc::RENAME_SWAP,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_no_replace(parent: RawFd, source: &CStr, target: &CStr) -> Result<(), AppBoundPathError> {
    if unsafe {
        libc::renameat2(
            parent,
            source.as_ptr(),
            parent,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_exchange(parent: RawFd, source: &CStr, target: &CStr) -> Result<(), AppBoundPathError> {
    if unsafe {
        libc::renameat2(
            parent,
            source.as_ptr(),
            parent,
            target.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn rename_no_replace(
    _parent: RawFd,
    _source: &CStr,
    _target: &CStr,
) -> Result<(), AppBoundPathError> {
    Err(AppBoundPathError::UnsupportedPlatform)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn rename_exchange(
    _parent: RawFd,
    _source: &CStr,
    _target: &CStr,
) -> Result<(), AppBoundPathError> {
    Err(AppBoundPathError::UnsupportedPlatform)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("magician-bound-path-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn params(values: &[(&str, Value)]) -> HashMap<String, Value> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn read_retains_exact_file_and_refuses_symlink_substitution() {
        let root = temp_root();
        fs::write(root.join("safe.txt"), "safe").unwrap();
        let action = ExecutableAction::File(FileAction::Read {
            path: PathBuf::from("safe.txt"),
            encoding: None,
        });
        let owner = PreparedAppBoundPath::prepare(&action, &params(&[]), &root).unwrap();
        // Intentional hostile-fixture rename: this simulates target substitution;
        // it is not a store publication path and must bypass the durable writer.
        fs::rename(root.join("safe.txt"), root.join("old.txt")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("old.txt", root.join("safe.txt")).unwrap();
        assert!(matches!(
            owner.verify_current(),
            Err(AppBoundPathError::UnsafeTarget
                | AppBoundPathError::IdentityChanged
                | AppBoundPathError::Io(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hardlinks_and_parent_components_are_refused() {
        let root = temp_root();
        fs::write(root.join("source.txt"), "secret").unwrap();
        fs::hard_link(root.join("source.txt"), root.join("alias.txt")).unwrap();
        let hardlink = ExecutableAction::File(FileAction::Read {
            path: PathBuf::from("alias.txt"),
            encoding: None,
        });
        assert!(matches!(
            PreparedAppBoundPath::prepare(&hardlink, &params(&[]), &root),
            Err(AppBoundPathError::UnsafeTarget)
        ));
        let escape = ExecutableAction::File(FileAction::Read {
            path: PathBuf::from("../source.txt"),
            encoding: None,
        });
        assert!(matches!(
            PreparedAppBoundPath::prepare(&escape, &params(&[]), &root),
            Err(AppBoundPathError::InvalidRelativePath)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_is_bounded_and_uses_create_new_then_replace_semantics() {
        let root = temp_root();
        let make = |content: &str| {
            ExecutableAction::File(FileAction::Write {
                path: PathBuf::from("result.txt"),
                content: content.to_owned(),
                create_dirs: false,
            })
        };
        PreparedAppBoundPath::prepare(&make("one"), &params(&[]), &root)
            .unwrap()
            .execute_file()
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("result.txt")).unwrap(), "one");
        PreparedAppBoundPath::prepare(&make("two"), &params(&[]), &root)
            .unwrap()
            .execute_file()
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("result.txt")).unwrap(), "two");
        let oversized = make(&"x".repeat((APP_BOUND_FILE_CONTENT_CEILING + 1) as usize));
        assert!(matches!(
            PreparedAppBoundPath::prepare(&oversized, &params(&[]), &root),
            Err(AppBoundPathError::ByteCeiling)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_target_substitution_is_refused_without_overwriting_substitute() {
        let root = temp_root();
        fs::write(root.join("result.txt"), "reviewed").unwrap();
        let action = ExecutableAction::File(FileAction::Write {
            path: PathBuf::from("result.txt"),
            content: "new".to_owned(),
            create_dirs: false,
        });
        let owner = PreparedAppBoundPath::prepare(&action, &params(&[]), &root).unwrap();
        // Intentional hostile-fixture rename: replace the reviewed inode before
        // execution so the retained descriptor must reject the substitute.
        fs::rename(root.join("result.txt"), root.join("reviewed.txt")).unwrap();
        fs::write(root.join("result.txt"), "substitute").unwrap();
        assert!(matches!(
            owner.execute_file(),
            Err(AppBoundPathError::IdentityChanged)
        ));
        assert_eq!(
            fs::read_to_string(root.join("result.txt")).unwrap(),
            "substitute"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_new_replay_and_parent_rename_are_refused() {
        let root = temp_root();
        fs::create_dir(root.join("reviewed")).unwrap();
        let action = ExecutableAction::File(FileAction::Write {
            path: PathBuf::from("reviewed/result.txt"),
            content: "new".to_owned(),
            create_dirs: false,
        });
        let replay = PreparedAppBoundPath::prepare(&action, &params(&[]), &root).unwrap();
        fs::write(root.join("reviewed/result.txt"), "racer").unwrap();
        assert!(matches!(
            replay.execute_file(),
            Err(AppBoundPathError::IdentityChanged)
        ));
        assert_eq!(
            fs::read_to_string(root.join("reviewed/result.txt")).unwrap(),
            "racer"
        );

        fs::remove_file(root.join("reviewed/result.txt")).unwrap();
        let renamed = PreparedAppBoundPath::prepare(&action, &params(&[]), &root).unwrap();
        // Intentional hostile-fixture rename: move the reviewed parent out from
        // under the retained capability; this does not publish durable state.
        fs::rename(root.join("reviewed"), root.join("moved")).unwrap();
        fs::create_dir(root.join("reviewed")).unwrap();
        assert!(matches!(
            renamed.execute_file(),
            Err(AppBoundPathError::IdentityChanged)
        ));
        assert!(!root.join("reviewed/result.txt").exists());
        assert!(!root.join("moved/result.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn table_owner_rewrites_only_to_retained_descriptor() {
        let root = temp_root();
        fs::write(root.join("rows.csv"), "id,name\n1,Ada\n").unwrap();
        let parameters = params(&[
            ("__action_name", Value::String("preview".into())),
            ("source", Value::String("rows.csv".into())),
            ("limit", Value::from(5)),
        ]);
        let action = ExecutableAction::DuckDb(DuckDbAction {
            sql: "ambient SQL must be discarded".into(),
            database: None,
            output_format: "json".into(),
            timeout_secs: Some(5),
        });
        let owner = PreparedAppBoundPath::prepare(&action, &parameters, &root).unwrap();
        let action = owner.bound_table_action().unwrap();
        assert!(action.sql.contains("read_csv_auto"));
        assert!(!action.sql.contains(root.to_string_lossy().as_ref()));
        assert!(action.database.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn table_provider_reads_the_retained_descriptor_end_to_end() {
        use crate::magician_v2::execution::{
            capability::CapabilityProvider, compiled_providers::DuckDbCapabilityProvider,
        };

        let root = temp_root();
        fs::write(root.join("rows.csv"), "id,name\n1,Ada\n").unwrap();
        let parameters = params(&[
            ("__action_name", Value::String("preview".into())),
            ("source", Value::String("rows.csv".into())),
            ("limit", Value::from(5)),
        ]);
        let lowered = ExecutableAction::DuckDb(DuckDbAction {
            sql: "discarded".into(),
            database: None,
            output_format: "json".into(),
            timeout_secs: Some(5),
        });
        let owner = PreparedAppBoundPath::prepare(&lowered, &parameters, &root).unwrap();
        let exact = ExecutableAction::DuckDb(owner.bound_table_action().unwrap());
        let result = DuckDbCapabilityProvider::new()
            .unwrap()
            .execute(&exact, None, 5)
            .await
            .unwrap();
        assert!(result.as_text().is_some_and(|text| text.contains("Ada")));
        drop(owner);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_write_is_an_error_before_atomic_publication() {
        struct PartialWriter {
            written: Vec<u8>,
            first: bool,
        }

        impl Write for PartialWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.first {
                    self.first = false;
                    let count = bytes.len().min(3);
                    self.written.extend_from_slice(&bytes[..count]);
                    Ok(count)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "injected partial write",
                    ))
                }
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut writer = PartialWriter {
            written: Vec::new(),
            first: true,
        };
        assert!(write_complete(&mut writer, b"complete payload").is_err());
        assert_eq!(writer.written, b"com");
    }
}
