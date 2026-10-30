//! Opt-in public, static MCP profiles. Documents come from reviewed tool
//! metadata; neither request arguments nor public HTTP requests choose files.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const PUBLIC_PROFILE_PREFIX: &str = "/.well-known/magican/mcp-profiles/";
pub const MAX_PUBLIC_PROFILE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpPublicProfile {
    /// Native argument location for the installation-owned public profile URL.
    pub argument_pointer: String,
    /// Literal public JSON only. No env interpolation or private file references.
    pub document: Value,
}

impl McpPublicProfile {
    pub fn validate(&self) -> Result<(), String> {
        pointer_parts(&self.argument_pointer)?;
        document_bytes(&self.document)?;
        Ok(())
    }

    /// Publish before dispatch so a provider can fetch the exact immutable
    /// document even if the next call runs in a different worker/process.
    pub fn publish(&self, storage: &Path, origin: &str) -> Result<String, String> {
        self.validate()?;
        let origin = public_https_origin(origin)?;
        let bytes = document_bytes(&self.document)?;
        let digest = digest(&bytes);
        let directory = profile_directory(storage, true)?;
        let destination = directory.join(format!("{digest}.json"));
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                if read_public_profile(storage, &digest)? != bytes {
                    return Err("public MCP profile content differs from its identity".into());
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut temp = tempfile::NamedTempFile::new_in(&directory)
                    .map_err(|_| "public MCP profile cannot be staged")?;
                temp.write_all(&bytes)
                    .and_then(|_| temp.as_file().sync_all())
                    .map_err(|_| "public MCP profile cannot be saved")?;
                if let Err(error) = temp.persist_noclobber(&destination) {
                    if error.error.kind() != std::io::ErrorKind::AlreadyExists
                        || read_public_profile(storage, &digest)? != bytes
                    {
                        return Err("public MCP profile cannot be published".into());
                    }
                }
            },
            Err(_) => return Err("public MCP profile storage is unavailable".into()),
        }
        Ok(format!("{origin}{PUBLIC_PROFILE_PREFIX}{digest}.json"))
    }

    /// Insert only the declared leaf. Preserve native metadata, and refuse a
    /// caller-supplied identity that differs from this installation's URL.
    pub fn bind(&self, arguments: &mut Value, url: &str) -> Result<(), String> {
        let parts = pointer_parts(&self.argument_pointer)?;
        let mut target = arguments;
        for (index, key) in parts.iter().enumerate() {
            let object = target
                .as_object_mut()
                .ok_or("public MCP profile argument ancestors must be objects")?;
            if index + 1 == parts.len() {
                if let Some(existing) = object.get(key) {
                    if existing.as_str() != Some(url) {
                        return Err(
                            "MCP profile must use this installation's public profile URL".into(),
                        );
                    }
                } else {
                    object.insert(key.clone(), Value::String(url.to_owned()));
                }
                return Ok(());
            }
            target = object
                .entry(key.clone())
                .or_insert_with(|| Value::Object(Default::default()));
        }
        Err("public MCP profile argument pointer is invalid".into())
    }
}

fn pointer_parts(pointer: &str) -> Result<Vec<String>, String> {
    if !pointer.starts_with('/') || pointer.len() > 1024 {
        return Err("public MCP profile requires a bounded JSON pointer".into());
    }
    let mut parts = Vec::new();
    for raw in pointer[1..].split('/') {
        let mut decoded = String::new();
        let mut chars = raw.chars();
        while let Some(character) = chars.next() {
            if character == '~' {
                decoded.push(match chars.next() {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ => return Err("public MCP profile pointer escape is invalid".into()),
                });
            } else {
                decoded.push(character);
            }
        }
        if decoded.is_empty() || parts.len() >= 16 {
            return Err("public MCP profile pointer exceeds local bounds".into());
        }
        parts.push(decoded);
    }
    Ok(parts)
}

fn document_bytes(document: &Value) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(document).map_err(|_| "public MCP profile JSON is invalid")?;
    if !document.is_object() || bytes.len() > MAX_PUBLIC_PROFILE_BYTES {
        return Err("public MCP profile must be a bounded JSON object".into());
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn public_https_origin(origin: &str) -> Result<String, String> {
    let parsed = url::Url::parse(origin).map_err(|_| "public MCP profile origin is invalid")?;
    let host = parsed.host_str().unwrap_or_default().trim_end_matches('.');
    let local_v4 = |ip: std::net::Ipv4Addr| {
        ip.is_private()
            || ip.is_loopback()
            || ip.is_link_local()
            || ip.is_unspecified()
            || ip.is_multicast()
            || ip.is_broadcast()
            || ip.is_documentation()
            || ip.octets()[0] == 0
            || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
    };
    let local_ip = parsed.host().is_some_and(|host| match host {
        url::Host::Ipv4(ip) => local_v4(ip),
        url::Host::Ipv6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.to_ipv4_mapped().is_some_and(local_v4)
        },
        _ => false,
    });
    if parsed.scheme() != "https"
        || host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || local_ip
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("public MCP profiles require this installation's public HTTPS origin; complete remote-access setup first".into());
    }
    Ok(parsed.origin().ascii_serialization())
}

fn profile_directory(storage: &Path, create: bool) -> Result<PathBuf, String> {
    let mut path =
        fs::canonicalize(storage).map_err(|_| "public MCP profile storage is unavailable")?;
    for part in ["system", "mcp-public-profiles"] {
        path.push(part);
        if create {
            match fs::create_dir(&path) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
                Err(_) => return Err("public MCP profile directory cannot be created".into()),
            }
        }
        if !fs::symlink_metadata(&path)
            .map_err(|_| "public MCP profile directory is unavailable")?
            .is_dir()
        {
            return Err("public MCP profile directory must be a real directory".into());
        }
    }
    Ok(path)
}

/// Public HTTP exposes only a content-addressed JSON file from this dedicated
/// store. Verify both bounds and digest before returning anything to a caller.
pub fn read_public_profile(storage: &Path, id: &str) -> Result<Vec<u8>, String> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("public MCP profile identity is invalid".into());
    }
    let path = profile_directory(storage, false)?.join(format!("{id}.json"));
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| "public MCP profile is unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "public MCP profile is unavailable")?
        .is_file()
    {
        return Err("public MCP profile must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PUBLIC_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "public MCP profile cannot be read")?;
    if bytes.len() > MAX_PUBLIC_PROFILE_BYTES || digest(&bytes) != id {
        return Err("public MCP profile content is invalid".into());
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "public MCP profile JSON is invalid")?;
    document_bytes(&value)?;
    Ok(bytes)
}
