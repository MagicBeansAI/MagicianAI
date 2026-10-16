use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::ffi::CString;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::ffi::OsStrExt;

use anydoc::{ConvertError, Format};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;

const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INLINE_CHARS: usize = 16 * 1024 * 1024;
// Leave worst-case headroom below the governed 32 MiB stdout ceiling: a JSON
// control character can expand to a six-byte `\uXXXX` escape.
const MAX_INLINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_EXPORTED_ASSETS: usize = 2_048;
const MAX_ASSET_REPORT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "document-to-markdown",
    version,
    about = "Convert supported documents to GitHub-Flavored Markdown with AnyDoc"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Convert one local document to Markdown.
    Convert(ConvertArgs),
}

#[derive(Debug, Args)]
struct ConvertArgs {
    /// Local input document. Maximum size is 128 MiB.
    #[arg(long)]
    input_file: PathBuf,

    /// Write Markdown to this file. Without this option, content is returned in JSON.
    #[arg(long)]
    output_file: Option<PathBuf>,

    /// Override detection using a supported extension name, or use `auto`.
    #[arg(long, default_value = "auto")]
    format: String,

    /// Export embedded assets. PDFs do not expose AnyDoc's document model.
    #[arg(long)]
    assets_dir: Option<PathBuf>,

    /// Include Markdown in JSON even when --output-file is present.
    #[arg(long)]
    include_content: bool,

    /// Limit Markdown characters returned inline. A 4 MiB encoded ceiling also
    /// applies; file output is never truncated.
    #[arg(long)]
    max_chars: Option<usize>,

    /// Pretty-print the JSON response.
    #[arg(long)]
    pretty: bool,
}

#[derive(Debug, Serialize)]
struct ConversionOutput {
    ok: bool,
    method: &'static str,
    format: &'static str,
    chars: usize,
    bytes: usize,
    content_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    assets: Vec<AssetOutput>,
    preview_hint: String,
}

#[derive(Debug, Clone, Serialize)]
struct AssetOutput {
    id: usize,
    media_type: String,
    path: String,
    bytes: usize,
}

#[derive(Debug)]
struct PlannedAsset {
    output: AssetOutput,
    filename: String,
    contents: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(Debug)]
struct AppError {
    code: String,
    message: String,
}

impl AppError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn io(operation: &str, path: &Path, error: io::Error) -> Self {
        Self::new(
            "io",
            format!("{operation} `{}` failed: {error}", path.display()),
        )
    }
}

impl From<ConvertError> for AppError {
    fn from(error: ConvertError) -> Self {
        let code = snake_case_error_code(error.code());
        Self::new(code, error.to_string())
    }
}

fn snake_case_error_code(code: &str) -> String {
    let mut normalized = String::with_capacity(code.len() + 4);
    for byte in code.bytes() {
        if byte.is_ascii_uppercase() {
            if !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push(char::from(byte.to_ascii_lowercase()));
        } else {
            normalized.push(char::from(byte));
        }
    }
    normalized
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Convert(args) => {
            let pretty = args.pretty;
            match convert(args) {
                Ok(output) => match write_json(io::stdout().lock(), &output, pretty) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                    Err(error) => {
                        emit_error(&AppError::new(
                            "io",
                            format!("writing stdout failed: {error}"),
                        ));
                        ExitCode::FAILURE
                    },
                },
                Err(error) => {
                    emit_error(&error);
                    ExitCode::FAILURE
                },
            }
        },
    }
}

fn convert(args: ConvertArgs) -> Result<ConversionOutput, AppError> {
    validate_path_argument(&args.input_file, "input_file")?;
    if let Some(path) = args.output_file.as_deref() {
        validate_path_argument(path, "output_file")?;
    }
    if let Some(path) = args.assets_dir.as_deref() {
        validate_path_argument(path, "assets_dir")?;
    }
    if let Some(max_chars) = args.max_chars
        && !(1..=MAX_INLINE_CHARS).contains(&max_chars)
    {
        return Err(AppError::new(
            "invalid_max_chars",
            format!("max_chars must be between 1 and {MAX_INLINE_CHARS}"),
        ));
    }
    reject_input_output_collision(&args.input_file, args.output_file.as_deref())?;
    let bytes = read_bounded_regular_file(&args.input_file)?;
    let format = resolve_format(&args.format, &bytes, &args.input_file)?;

    if args.assets_dir.is_some() && format == Format::Pdf {
        return Err(AppError::new(
            "assets_unsupported",
            "embedded asset export is unavailable for PDF because AnyDoc emits PDF Markdown directly",
        ));
    }

    let markdown = anydoc::to_markdown_bytes(&bytes, format).map_err(AppError::from)?;
    let planned_assets = match args.assets_dir.as_deref() {
        Some(directory) => plan_assets(&bytes, format, &args.input_file, directory)?,
        None => Vec::new(),
    };
    publish_conversion(
        args.output_file.as_deref(),
        markdown.as_bytes(),
        args.assets_dir.as_deref(),
        &planned_assets,
    )?;
    let assets = planned_assets
        .iter()
        .map(|asset| asset.output.clone())
        .collect::<Vec<_>>();

    let chars = markdown.chars().count();
    let markdown_bytes = markdown.len();
    let inline_limit = args.max_chars.unwrap_or(MAX_INLINE_CHARS);
    let content_truncated = chars > inline_limit || markdown_bytes > MAX_INLINE_BYTES;
    let content = if args.output_file.is_none() || args.include_content {
        if content_truncated {
            Some(take_bounded_inline(
                markdown,
                inline_limit,
                MAX_INLINE_BYTES,
            ))
        } else {
            Some(markdown)
        }
    } else {
        None
    };
    let output_path = args
        .output_file
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned());
    let preview_hint = match (&output_path, assets.len()) {
        (Some(path), 0) => format!("Converted {chars} Markdown characters to {path}"),
        (Some(path), count) => {
            format!("Converted {chars} Markdown characters to {path}; exported {count} assets")
        },
        (None, 0) => format!("Converted document to {chars} Markdown characters"),
        (None, count) => {
            format!("Converted document to {chars} Markdown characters; exported {count} assets")
        },
    };

    Ok(ConversionOutput {
        ok: true,
        method: "anydoc",
        format: format_name(format),
        chars,
        bytes: markdown_bytes,
        content_truncated,
        content,
        output_path,
        assets,
        preview_hint,
    })
}

fn take_bounded_inline(mut markdown: String, max_chars: usize, max_bytes: usize) -> String {
    let mut end = 0;
    let mut characters = 0;
    for (index, character) in markdown.char_indices() {
        if characters == max_chars || index + character.len_utf8() > max_bytes {
            break;
        }
        characters += 1;
        end = index + character.len_utf8();
    }
    markdown.truncate(end);
    markdown
}

fn read_bounded_regular_file(path: &Path) -> Result<Vec<u8>, AppError> {
    let mut file = File::open(path).map_err(|error| AppError::io("opening", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| AppError::io("reading metadata for", path, error))?;
    if !metadata.is_file() {
        return Err(AppError::new(
            "invalid_input",
            format!("input `{}` is not a regular file", path.display()),
        ));
    }
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(AppError::new(
            "input_too_large",
            format!(
                "input `{}` is {} bytes; maximum is {MAX_INPUT_BYTES} bytes",
                path.display(),
                metadata.len()
            ),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_INPUT_BYTES) as usize);
    Read::take(&mut file, MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| AppError::io("reading", path, error))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(AppError::new(
            "input_too_large",
            format!(
                "input `{}` grew beyond the {MAX_INPUT_BYTES} byte maximum while being read",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

fn validate_path_argument(path: &Path, label: &str) -> Result<(), AppError> {
    let length = path.as_os_str().len();
    if length == 0 || length > MAX_PATH_BYTES {
        return Err(AppError::new(
            "invalid_path",
            format!("{label} must be between 1 and {MAX_PATH_BYTES} encoded bytes"),
        ));
    }
    Ok(())
}

fn resolve_format(raw: &str, bytes: &[u8], path: &Path) -> Result<Format, AppError> {
    let normalized = raw.trim().trim_start_matches('.');
    if !normalized.eq_ignore_ascii_case("auto") {
        return Format::from_extension(normalized).ok_or_else(|| {
            AppError::new(
                "unsupported_format",
                format!("unsupported format override `{raw}`"),
            )
        });
    }

    Format::from_bytes(bytes)
        .or_else(|| Format::from_path(path))
        .ok_or_else(|| {
            AppError::new(
                "unsupported",
                format!(
                    "unrecognized document content and extension for `{}`",
                    path.display()
                ),
            )
        })
}

fn reject_input_output_collision(input: &Path, output: Option<&Path>) -> Result<(), AppError> {
    let Some(output) = output else {
        return Ok(());
    };
    let input_canonical =
        fs::canonicalize(input).map_err(|error| AppError::io("resolving input", input, error))?;
    if let Ok(output_canonical) = fs::canonicalize(output)
        && output_canonical == input_canonical
    {
        return Err(AppError::new(
            "output_is_input",
            "output_file must not overwrite the input document",
        ));
    }
    Ok(())
}

fn plan_assets(
    bytes: &[u8],
    format: Format,
    input: &Path,
    directory: &Path,
) -> Result<Vec<PlannedAsset>, AppError> {
    let document = anydoc::to_document(bytes, format).map_err(AppError::from)?;
    if document.assets.len() > MAX_EXPORTED_ASSETS {
        return Err(AppError::new(
            "too_many_assets",
            format!(
                "document contains {} assets; maximum is {MAX_EXPORTED_ASSETS}",
                document.assets.len()
            ),
        ));
    }
    let stem = safe_component(&input.file_stem().unwrap_or_default().to_string_lossy());
    let mut planned = Vec::with_capacity(document.assets.len());
    let mut names = BTreeSet::new();
    let mut report_bytes = 0usize;
    for asset in &document.assets {
        let extension = asset_extension(&asset.media_type);
        let filename = format!("{stem}-{}.{}", asset.id.0, extension);
        if !names.insert(filename.clone()) {
            return Err(AppError::new(
                "duplicate_asset",
                "document produced duplicate asset identifiers",
            ));
        }
        let path = directory.join(&filename);
        let display = path.to_string_lossy().into_owned();
        report_bytes = report_bytes
            .saturating_add(display.len())
            .saturating_add(asset.media_type.len());
        if report_bytes > MAX_ASSET_REPORT_BYTES {
            return Err(AppError::new(
                "asset_report_too_large",
                format!("asset metadata exceeds {MAX_ASSET_REPORT_BYTES} encoded bytes"),
            ));
        }
        planned.push((filename, display));
    }
    Ok(document
        .assets
        .into_iter()
        .zip(planned)
        .map(|(asset, (filename, display))| PlannedAsset {
            output: AssetOutput {
                id: asset.id.0,
                media_type: asset.media_type,
                path: display,
                bytes: asset.bytes.len(),
            },
            filename,
            contents: asset.bytes,
        })
        .collect())
}

fn safe_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .take(80)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "document".to_string()
    } else {
        cleaned
    }
}

fn asset_extension(media_type: &str) -> String {
    let subtype = media_type
        .split_once('/')
        .map(|(_, subtype)| subtype)
        .unwrap_or("");
    let extension: String = subtype
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    if extension.is_empty() {
        "bin".to_string()
    } else {
        extension.to_ascii_lowercase()
    }
}

fn publish_conversion(
    output_file: Option<&Path>,
    markdown: &[u8],
    assets_dir: Option<&Path>,
    assets: &[PlannedAsset],
) -> Result<(), AppError> {
    if let Some(path) = output_file
        && fs::symlink_metadata(path).is_ok()
    {
        return Err(AppError::new(
            "destination_exists",
            format!("output destination `{}` already exists", path.display()),
        ));
    }
    if let Some(path) = assets_dir
        && fs::symlink_metadata(path).is_ok()
    {
        return Err(AppError::new(
            "destination_exists",
            format!("asset destination `{}` already exists", path.display()),
        ));
    }
    if let (Some(output), Some(directory)) = (output_file, assets_dir) {
        let output = absolute_lexical(output)?;
        let directory = absolute_lexical(directory)?;
        if output.starts_with(&directory) {
            return Err(AppError::new(
                "destination_collision",
                "output_file must not be inside assets_dir",
            ));
        }
    }

    let mut output_staging = match output_file {
        Some(path) => Some(stage_file(path, markdown)?),
        None => None,
    };
    let mut assets_staging = match assets_dir {
        Some(directory) => {
            let parent = existing_parent(directory)?;
            let staging = tempfile::tempdir_in(parent)
                .map_err(|error| AppError::io("creating asset staging directory", parent, error))?;
            for asset in assets {
                let path = staging.path().join(&asset.filename);
                fs::write(&path, &asset.contents)
                    .map_err(|error| AppError::io("writing staged asset", &path, error))?;
            }
            Some(staging)
        },
        None => None,
    };

    let mut output_committed = false;
    if let (Some(path), Some(staging)) = (output_file, output_staging.take()) {
        staging
            .persist_noclobber(path)
            .map_err(|error| AppError::io("publishing Markdown output", path, error.error))?;
        output_committed = true;
    }

    if let (Some(directory), Some(staging)) = (assets_dir, assets_staging.take()) {
        if let Err(error) = publish_asset_directory(directory, staging) {
            if output_committed && let Some(path) = output_file {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
    }
    Ok(())
}

fn existing_parent(path: &Path) -> Result<&Path, AppError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !fs::metadata(parent).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(AppError::new(
            "invalid_destination",
            format!(
                "destination parent `{}` is not an existing directory",
                parent.display()
            ),
        ));
    }
    Ok(parent)
}

fn stage_file(path: &Path, contents: &[u8]) -> Result<tempfile::NamedTempFile, AppError> {
    let parent = existing_parent(path)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| AppError::io("creating temporary output", parent, error))?;
    temporary
        .write_all(contents)
        .map_err(|error| AppError::io("writing temporary output", path, error))?;
    temporary
        .flush()
        .map_err(|error| AppError::io("flushing temporary output", path, error))?;
    Ok(temporary)
}

fn publish_asset_directory(directory: &Path, staging: tempfile::TempDir) -> Result<(), AppError> {
    let staged_path = staging.keep();
    if let Err(error) = rename_directory_noclobber(&staged_path, directory) {
        let _ = fs::remove_dir_all(&staged_path);
        return Err(AppError::io("publishing asset directory", directory, error));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_directory_noclobber(source: &Path, destination: &Path) -> io::Result<()> {
    const RENAME_EXCL: u32 = 0x0000_0004;
    unsafe extern "C" {
        fn renameatx_np(
            from_fd: libc::c_int,
            from: *const libc::c_char,
            to_fd: libc::c_int,
            to: *const libc::c_char,
            flags: libc::c_uint,
        ) -> libc::c_int;
    }
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: both C strings remain alive for the call. RENAME_EXCL gives the
    // directory publish the same create-only contract as persist_noclobber.
    let result = unsafe {
        renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_directory_noclobber(source: &Path, destination: &Path) -> io::Result<()> {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: renameat2 is invoked with valid, live C strings and the
    // create-only flag; it performs no memory access beyond those strings.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_directory_noclobber(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic create-only directory publishing is unsupported on this platform",
    ))
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| AppError::io("resolving destination", path, error))
    }
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Doc => "doc",
        Format::Docx => "docx",
        Format::Odt => "odt",
        Format::Pdf => "pdf",
        Format::Ppt => "ppt",
        Format::Pptx => "pptx",
        Format::Rtf => "rtf",
        Format::Epub => "epub",
        Format::Excel => "excel",
        Format::Ods => "ods",
        Format::Odp => "odp",
        Format::Csv => "csv",
    }
}

fn write_json(mut writer: impl Write, value: &impl Serialize, pretty: bool) -> io::Result<()> {
    if pretty {
        serde_json::to_writer_pretty(&mut writer, value)?;
    } else {
        serde_json::to_writer(&mut writer, value)?;
    }
    writer.write_all(b"\n")
}

fn emit_error(error: &AppError) {
    let envelope = ErrorEnvelope {
        ok: false,
        error: ErrorBody {
            code: &error.code,
            message: &error.message,
        },
    };
    let _ = write_json(io::stderr().lock(), &envelope, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use tempfile::tempdir;

    fn args(input_file: PathBuf) -> ConvertArgs {
        ConvertArgs {
            input_file,
            output_file: None,
            format: "auto".to_string(),
            assets_dir: None,
            include_content: false,
            max_chars: None,
            pretty: false,
        }
    }

    #[test]
    fn cli_accepts_the_supported_conversion_options() {
        let cli = Cli::try_parse_from([
            "document-to-markdown",
            "convert",
            "--input-file",
            "input.docx",
            "--output-file",
            "output.md",
            "--format",
            "docx",
            "--assets-dir",
            "assets",
            "--include-content",
            "--max-chars",
            "4096",
            "--pretty",
        ])
        .unwrap();
        let Command::Convert(parsed) = cli.command;
        assert_eq!(parsed.input_file, PathBuf::from("input.docx"));
        assert_eq!(parsed.output_file, Some(PathBuf::from("output.md")));
        assert_eq!(parsed.format, "docx");
        assert_eq!(parsed.assets_dir, Some(PathBuf::from("assets")));
        assert!(parsed.include_content);
        assert_eq!(parsed.max_chars, Some(4096));
        assert!(parsed.pretty);
    }

    #[test]
    fn csv_converts_to_markdown_with_content_detection_fallback() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("people.csv");
        fs::write(&input, "name,age\nAda,36\nGrace,45\n").unwrap();

        let output = convert(args(input)).unwrap();

        assert_eq!(output.format, "csv");
        let markdown = output.content.unwrap();
        assert!(markdown.contains("Ada"));
        assert!(markdown.contains("Grace"));
        assert!(markdown.contains('|'));
    }

    #[test]
    fn explicit_format_supports_extensionless_csv() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("upload");
        fs::write(&input, "name,age\nAda,36\n").unwrap();
        let mut request = args(input);
        request.format = "csv".to_string();

        let output = convert(request).unwrap();

        assert_eq!(output.format, "csv");
        assert!(output.content.unwrap().contains("Ada"));
    }

    #[test]
    fn output_file_is_atomic_and_omits_duplicate_content_by_default() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("note.rtf");
        let output_path = directory.path().join("nested/note.md");
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(&input, r"{\rtf1\ansi Hello from RTF}").unwrap();
        let mut request = args(input);
        request.output_file = Some(output_path.clone());

        let output = convert(request).unwrap();

        assert!(output.content.is_none());
        assert!(
            fs::read_to_string(output_path)
                .unwrap()
                .contains("Hello from RTF")
        );
    }

    #[test]
    fn include_content_keeps_markdown_with_an_output_file() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("note.rtf");
        fs::write(&input, r"{\rtf1\ansi Hello}").unwrap();
        let mut request = args(input);
        request.output_file = Some(directory.path().join("note.md"));
        request.include_content = true;

        let output = convert(request).unwrap();

        assert!(output.content.unwrap().contains("Hello"));
    }

    #[test]
    fn max_chars_bounds_inline_content_without_truncating_the_output_file() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("people.csv");
        let output_path = directory.path().join("people.md");
        fs::write(&input, "name,description\nAda,mathematician\n").unwrap();
        let mut request = args(input);
        request.output_file = Some(output_path.clone());
        request.include_content = true;
        request.max_chars = Some(8);

        let output = convert(request).unwrap();

        assert!(output.content_truncated);
        assert_eq!(output.content.unwrap().chars().count(), 8);
        assert!(fs::read_to_string(output_path).unwrap().chars().count() > 8);
    }

    #[test]
    fn rejects_unbounded_inline_limit_values() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("people.csv");
        fs::write(&input, "name\nAda\n").unwrap();
        let mut request = args(input);
        request.max_chars = Some(0);

        let error = convert(request).unwrap_err();

        assert_eq!(error.code, "invalid_max_chars");
    }

    #[test]
    fn anydoc_error_codes_match_the_governed_failure_vocabulary() {
        assert_eq!(snake_case_error_code("unsupported"), "unsupported");
        assert_eq!(snake_case_error_code("resourceLimit"), "resource_limit");
        assert_eq!(snake_case_error_code("missingPart"), "missing_part");
        for code in ["unsupported", "resource_limit", "missing_part"] {
            assert!(
                code.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            );
        }
    }

    #[test]
    fn path_arguments_have_an_independent_cli_boundary() {
        assert!(validate_path_argument(Path::new("input.docx"), "input_file").is_ok());
        let oversized = PathBuf::from("x".repeat(MAX_PATH_BYTES + 1));
        assert_eq!(
            validate_path_argument(&oversized, "output_file")
                .unwrap_err()
                .code,
            "invalid_path"
        );
    }

    #[test]
    fn inline_content_is_bounded_by_characters_and_encoded_bytes() {
        assert_eq!(take_bounded_inline("abcdef".to_string(), 3, 64), "abc");
        assert_eq!(take_bounded_inline("ééé".to_string(), 8, 4), "éé");
    }

    #[test]
    fn refuses_to_overwrite_the_input_document() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("people.csv");
        fs::write(&input, "name\nAda\n").unwrap();
        let mut request = args(input.clone());
        request.output_file = Some(input);

        let error = convert(request).unwrap_err();

        assert_eq!(error.code, "output_is_input");
    }

    #[test]
    fn rejects_oversized_input_before_allocating_it() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("huge.docx");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&input)
            .unwrap();
        file.set_len(MAX_INPUT_BYTES + 1).unwrap();

        let error = convert(args(input)).unwrap_err();

        assert_eq!(error.code, "input_too_large");
    }

    #[test]
    fn reports_unknown_documents_as_unsupported() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("unknown.bin");
        fs::write(&input, b"not a supported document").unwrap();

        let error = convert(args(input)).unwrap_err();

        assert_eq!(error.code, "unsupported");
    }

    #[test]
    fn rejects_pdf_asset_export_before_conversion() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("sample.pdf");
        fs::write(&input, b"%PDF-1.4\n").unwrap();
        let mut request = args(input);
        request.assets_dir = Some(directory.path().join("assets"));

        let error = convert(request).unwrap_err();

        assert_eq!(error.code, "assets_unsupported");
    }

    #[test]
    fn asset_names_cannot_escape_the_destination() {
        assert_eq!(safe_component("../strange name"), "___strange_name");
        assert_eq!(asset_extension("image/svg+xml"), "svgxml");
        assert_eq!(asset_extension("application/"), "bin");
    }

    #[test]
    fn existing_destinations_are_never_modified() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("existing.md");
        let assets = directory.path().join("existing-assets");
        fs::write(&output, "keep me").unwrap();
        fs::create_dir(&assets).unwrap();
        fs::write(assets.join("keep.bin"), b"keep asset").unwrap();

        let output_error = publish_conversion(Some(&output), b"replacement", None, &[])
            .expect_err("existing Markdown destination must fail closed");
        let assets_error = publish_conversion(None, b"ignored", Some(&assets), &[])
            .expect_err("existing asset destination must fail closed");

        assert_eq!(output_error.code, "destination_exists");
        assert_eq!(assets_error.code, "destination_exists");
        assert_eq!(fs::read_to_string(output).unwrap(), "keep me");
        assert_eq!(fs::read(assets.join("keep.bin")).unwrap(), b"keep asset");
    }

    #[test]
    fn asset_directory_is_published_as_one_complete_tree() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("assets");
        let asset = PlannedAsset {
            output: AssetOutput {
                id: 7,
                media_type: "image/png".to_string(),
                path: destination.join("page-7.png").display().to_string(),
                bytes: 4,
            },
            filename: "page-7.png".to_string(),
            contents: b"data".to_vec(),
        };

        publish_conversion(None, b"ignored", Some(&destination), &[asset]).unwrap();

        assert_eq!(fs::read(destination.join("page-7.png")).unwrap(), b"data");
    }

    #[test]
    fn staging_failure_leaves_no_output_or_asset_destination() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("result.md");
        let destination = directory.path().join("assets");
        let invalid_asset = PlannedAsset {
            output: AssetOutput {
                id: 1,
                media_type: "application/octet-stream".to_string(),
                path: destination.join("nested/fail.bin").display().to_string(),
                bytes: 4,
            },
            filename: "nested/fail.bin".to_string(),
            contents: b"data".to_vec(),
        };

        let error = publish_conversion(
            Some(&output),
            b"markdown",
            Some(&destination),
            &[invalid_asset],
        )
        .expect_err("invalid asset staging should fail the transaction");

        assert_eq!(error.code, "io");
        assert!(!output.exists());
        assert!(!destination.exists());
    }
}
