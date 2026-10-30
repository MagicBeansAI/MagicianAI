//! Host-owned quote state and an OS-held account lease. A stateless worker can
//! hand off to another process without losing quotes or allowing two checkouts.
use super::{Quote, QuoteCache, MAX_QUOTES};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "cart quote clock is invalid")?
        .as_millis()
        .try_into()
        .map_err(|_| "cart quote clock exceeds local bounds".into())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema_version: u32,
    next_claim: u64,
    quotes: Vec<((String, String), Quote)>,
}

fn open_private(path: &Path, create: bool) -> Result<File, String> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts
        .open(path)
        .map_err(|_| "cart quote state cannot be opened")?;
    if !file
        .metadata()
        .map_err(|_| "cart quote state metadata is unavailable")?
        .is_file()
    {
        return Err("cart quote state must be a regular file".into());
    }
    Ok(file)
}

pub(super) struct StoreLease {
    pub cache: QuoteCache,
    path: PathBuf,
    // Dropping this file releases the cross-process exclusion, including on a
    // crash. The persistent claimed bit continues to forbid a duplicate order.
    _lock: File,
}

impl StoreLease {
    pub fn open(storage: &Path, account: &str) -> Result<Self, String> {
        if account.len() != 64 || !account.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("cart quote account binding is invalid".into());
        }
        let root = storage.join("system/mcp-commerce");
        fs::create_dir_all(&root).map_err(|_| "cart quote directory cannot be created")?;
        if !fs::symlink_metadata(&root)
            .map_err(|_| "cart quote directory is unavailable")?
            .is_dir()
        {
            return Err("cart quote directory must not be a symlink".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|_| "cart quote directory cannot be made private")?;
        }
        let lock = open_private(&root.join(format!("{account}.lock")), true)?;
        lock.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                "another cart operation is in flight for this account; wait before retrying"
            } else {
                "cart quote account lease cannot be acquired"
            }
        })?;
        let path = root.join(format!("{account}.json"));
        let mut cache = QuoteCache::default();
        match open_private(&path, false) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_STATE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "cart quote state cannot be read")?;
                if bytes.len() as u64 > MAX_STATE_BYTES {
                    return Err("cart quote state exceeds local bounds".into());
                }
                let saved: Saved = serde_json::from_slice(&bytes)
                    .map_err(|_| "cart quote state is invalid; checkout is refused")?;
                if saved.schema_version != 1 || saved.quotes.len() > MAX_QUOTES {
                    return Err("cart quote state schema or count is invalid".into());
                }
                cache.next_claim = saved.next_claim;
                let now = now_ms()?;
                for (key, mut quote) in saved.quotes {
                    if quote.created_unix_ms > now.saturating_add(5000) {
                        return Err("cart quote clock moved backwards; checkout is refused".into());
                    }
                    quote.created = Instant::now()
                        .checked_sub(Duration::from_millis(
                            now.saturating_sub(quote.created_unix_ms),
                        ))
                        .ok_or("cart quote age is invalid")?;
                    if cache.entries.insert(key, quote).is_some() {
                        return Err("cart quote state has duplicate identifiers".into());
                    }
                }
                cache.prune(Instant::now());
            },
            Err(_)
                if fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {},
            Err(error) => return Err(error),
        }
        Ok(Self {
            cache,
            path,
            _lock: lock,
        })
    }

    pub fn save(&self) -> Result<(), String> {
        let saved = Saved {
            schema_version: 1,
            next_claim: self.cache.next_claim,
            quotes: self
                .cache
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        let bytes = serde_json::to_vec(&saved).map_err(|_| "cart quote state cannot be encoded")?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("cart quote state exceeds local bounds".into());
        }
        let root = self
            .path
            .parent()
            .ok_or("cart quote state has no directory")?;
        let temporary = root.join(format!(
            ".quote-{}-{}.tmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "cart quote clock is invalid")?
                .as_nanos()
        ));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = opts
            .open(&temporary)
            .map_err(|_| "cart quote state cannot be staged")?;
        let result = (|| {
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "cart quote state cannot be flushed")?;
            drop(file);
            // This private checkout-state writer intentionally retains its
            // create_new + 0600 + O_NOFOLLOW staging under the account lease.
            // It syncs both the unique temporary file and parent directory;
            // the generic writer does not supply these private-file flags.
            fs::rename(&temporary, &self.path)
                .map_err(|_| "cart quote state cannot be committed")?;
            #[cfg(unix)]
            {
                File::open(root)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| "cart quote directory cannot be flushed")?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
