//! Code-identity-rooted bootstrap client for the Android Apps desktop owner.
//!
//! The ordinary HTTP server never establishes first trust. On macOS the
//! runtime connects only to the exact private desktop-owned Unix socket,
//! verifies the live peer PID and its static code against the runtime's own
//! Apple Team ID, and only then exchanges the closed bootstrap DTOs.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use chrono::Utc;
use futures_util::StreamExt as _;
use magician_app_contract::android_owner::{
    AppAndroidOwnerBootstrapDesktopMessage, AppAndroidOwnerBootstrapRuntimeMessage,
    AppAndroidOwnerStatusReceipt, APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES,
    APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME,
    APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY,
    APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_SUPPORT_DIRECTORY,
};

use super::android_owner::{AppAndroidOwnerStore, AppAndroidOwnerStoreError};

const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
// The desktop readiness barrier is emitted before a bootstrap nonce exists.
// It includes the desktop's bounded blocking-slot admission plus strict
// live/static verification of the large runtime binary.
const PEER_READY_TIMEOUT: Duration = Duration::from_secs(45);
const OWNER_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const OWNER_STATUS_RESPONSE_CEILING: usize = 64 * 1024;
const OWNER_STATUS_URL: &str = "http://127.0.0.1:3017/host/apps/android/owner/status";

static OWNER_STATUS_CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();

#[derive(Debug)]
pub enum AppAndroidOwnerBootstrapError {
    Unavailable,
    PeerIdentityRejected,
    InvalidFrame,
    Store(AppAndroidOwnerStoreError),
    Io(String),
}

impl std::fmt::Display for AppAndroidOwnerBootstrapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for AppAndroidOwnerBootstrapError {}

impl From<AppAndroidOwnerStoreError> for AppAndroidOwnerBootstrapError {
    fn from(value: AppAndroidOwnerStoreError) -> Self {
        Self::Store(value)
    }
}

pub fn desktop_bootstrap_socket_path() -> Result<PathBuf, AppAndroidOwnerBootstrapError> {
    let root = dirs::data_dir().ok_or(AppAndroidOwnerBootstrapError::Unavailable)?;
    if !root.is_absolute() {
        return Err(AppAndroidOwnerBootstrapError::Unavailable);
    }
    Ok(root
        .join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_SUPPORT_DIRECTORY)
        .join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY)
        .join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME))
}

#[cfg(target_os = "macos")]
pub async fn run_once(
    store: Arc<AppAndroidOwnerStore>,
) -> Result<(), AppAndroidOwnerBootstrapError> {
    use tokio::{io::AsyncWriteExt as _, net::UnixStream};

    let socket_path = desktop_bootstrap_socket_path()?;
    let before = validate_private_socket_path(&socket_path)?;
    let mut stream = tokio::time::timeout(FRAME_TIMEOUT, UnixStream::connect(&socket_path))
        .await
        .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?
        .map_err(|error| AppAndroidOwnerBootstrapError::Io(error.to_string()))?;
    let after = validate_private_socket_path(&socket_path)?;
    if before != after {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    verify_connected_peer(&stream, socket_path.clone(), after).await?;

    let readiness: AppAndroidOwnerBootstrapDesktopMessage =
        read_frame_with_timeout(&mut stream, PEER_READY_TIMEOUT).await?;
    readiness
        .validate(Utc::now().timestamp_millis())
        .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    if !matches!(
        readiness,
        AppAndroidOwnerBootstrapDesktopMessage::PeerVerified { .. }
    ) {
        return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
    }

    // Recover or mint the correlation only after reciprocal strict code
    // validation. Besides avoiding charging that expensive validation to a
    // fresh nonce, this second ordering is important for retained pending
    // nonces: one that was fresh before code verification may have expired by
    // the time the desktop receives it. bootstrap_hello clears that unspent
    // expired state at the point of use, while retaining historical request or
    // rebind correlations that require an authenticated ExpiredUnspent reply.
    let retained_bootstrap = store.bootstrap_hello(Utc::now().timestamp_millis()).await?;
    let bootstrap = match retained_bootstrap {
        Some(value) => value,
        None => store.begin_bootstrap(Utc::now().timestamp_millis()).await?,
    };

    let recovery_hello = store.bootstrap_recovery_hello(&bootstrap).await?;
    let hello = match recovery_hello {
        Some(recovery) => AppAndroidOwnerBootstrapRuntimeMessage::recovery_hello(recovery),
        None => AppAndroidOwnerBootstrapRuntimeMessage::hello(bootstrap.clone()),
    };
    write_frame(&mut stream, &hello).await?;
    let first: AppAndroidOwnerBootstrapDesktopMessage = read_frame(&mut stream).await?;
    first
        .validate(Utc::now().timestamp_millis())
        .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    let completion = match first {
        AppAndroidOwnerBootstrapDesktopMessage::AwaitingNativeApproval {
            bootstrap_digest, ..
        } => {
            if bootstrap.digest().ok().as_deref() != Some(bootstrap_digest.as_str()) {
                return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
            }
            return Err(AppAndroidOwnerBootstrapError::Unavailable);
        },
        AppAndroidOwnerBootstrapDesktopMessage::ExpiredUnspent {
            bootstrap_digest, ..
        } => {
            if bootstrap.digest().ok().as_deref() != Some(bootstrap_digest.as_str())
                || bootstrap.expires_at_ms > Utc::now().timestamp_millis()
            {
                return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
            }
            store
                .abandon_expired_request(bootstrap, Utc::now().timestamp_millis())
                .await?;
            return Err(AppAndroidOwnerBootstrapError::Unavailable);
        },
        AppAndroidOwnerBootstrapDesktopMessage::RebindAwaitingNativeApproval { offer, .. } => {
            if offer.bootstrap != bootstrap {
                return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
            }
            store
                .retain_rebind_offer(offer, Utc::now().timestamp_millis())
                .await?;
            return Err(AppAndroidOwnerBootstrapError::Unavailable);
        },
        AppAndroidOwnerBootstrapDesktopMessage::ChallengeRequest { request, .. } => {
            if request.bootstrap != bootstrap {
                return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
            }
            let challenge = store
                .mint_bootstrap_challenge(request, Utc::now().timestamp_millis())
                .await?;
            write_frame(
                &mut stream,
                &AppAndroidOwnerBootstrapRuntimeMessage::challenge(challenge),
            )
            .await?;
            let response =
                read_frame::<AppAndroidOwnerBootstrapDesktopMessage>(&mut stream).await?;
            response
                .validate(Utc::now().timestamp_millis())
                .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
            match response {
                AppAndroidOwnerBootstrapDesktopMessage::Completion { completion, .. } => completion,
                _ => return Err(AppAndroidOwnerBootstrapError::InvalidFrame),
            }
        },
        AppAndroidOwnerBootstrapDesktopMessage::Completion { completion, .. } => completion,
        AppAndroidOwnerBootstrapDesktopMessage::FinalizedAck { .. } => {
            return Err(AppAndroidOwnerBootstrapError::InvalidFrame)
        },
        AppAndroidOwnerBootstrapDesktopMessage::PeerVerified { .. } => {
            return Err(AppAndroidOwnerBootstrapError::InvalidFrame)
        },
    };
    let status = store
        .complete_bootstrap(completion, Utc::now().timestamp_millis())
        .await?;
    let status_digest = status
        .digest()
        .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    write_frame(
        &mut stream,
        &AppAndroidOwnerBootstrapRuntimeMessage::finalized(status),
    )
    .await?;
    let acknowledgment = read_frame::<AppAndroidOwnerBootstrapDesktopMessage>(&mut stream).await?;
    acknowledgment
        .validate(Utc::now().timestamp_millis())
        .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    match acknowledgment {
        AppAndroidOwnerBootstrapDesktopMessage::FinalizedAck {
            status_digest: acknowledged,
            ..
        } if acknowledged == status_digest => {},
        _ => return Err(AppAndroidOwnerBootstrapError::InvalidFrame),
    }
    stream
        .shutdown()
        .await
        .map_err(|error| AppAndroidOwnerBootstrapError::Io(error.to_string()))?;
    store.mark_peer_identity_verified();
    revalidate_owner_status(&store).await?;
    store.mark_peer_verified();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub async fn run_once(
    _store: Arc<AppAndroidOwnerStore>,
) -> Result<(), AppAndroidOwnerBootstrapError> {
    Err(AppAndroidOwnerBootstrapError::Unavailable)
}

/// Compare the runtime's exact durable authority head with a fresh
/// desktop-signed, record-free high-water receipt. The literal-loopback
/// transport is not trusted for identity; only the pinned Ed25519 signature
/// established over the reciprocal code-identity UDS is authoritative.
pub async fn revalidate_owner_status(
    store: &AppAndroidOwnerStore,
) -> Result<(), AppAndroidOwnerBootstrapError> {
    let now_ms = Utc::now().timestamp_millis();
    let challenge = store.status_challenge(now_ms).await?;
    let client = OWNER_STATUS_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(OWNER_STATUS_TIMEOUT)
                .timeout(OWNER_STATUS_TIMEOUT)
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(AppAndroidOwnerBootstrapError::Unavailable)?;
    let response = client
        .post(OWNER_STATUS_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&challenge)
        .send()
        .await
        .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?;
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|length| length > OWNER_STATUS_RESPONSE_CEILING as u64)
    {
        return Err(AppAndroidOwnerBootstrapError::Unavailable);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?;
        if bytes.len().saturating_add(chunk.len()) > OWNER_STATUS_RESPONSE_CEILING {
            return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
        }
        bytes.extend_from_slice(&chunk);
    }
    let receipt: AppAndroidOwnerStatusReceipt =
        serde_json::from_slice(&bytes).map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    if let Err(error) = store
        .reconcile_status(&challenge, &receipt, Utc::now().timestamp_millis())
        .await
    {
        store.mark_peer_stale();
        return Err(error.into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn write_frame<T: serde::Serialize>(
    stream: &mut tokio::net::UnixStream,
    value: &T,
) -> Result<(), AppAndroidOwnerBootstrapError> {
    use tokio::io::AsyncWriteExt as _;

    let bytes =
        serde_json::to_vec(value).map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    if bytes.is_empty() || bytes.len() > APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES {
        return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?
        .to_be_bytes();
    tokio::time::timeout(FRAME_TIMEOUT, async {
        stream.write_all(&length).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?
    .map_err(|error| AppAndroidOwnerBootstrapError::Io(error.to_string()))
}

#[cfg(target_os = "macos")]
async fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut tokio::net::UnixStream,
) -> Result<T, AppAndroidOwnerBootstrapError> {
    read_frame_with_timeout(stream, FRAME_TIMEOUT).await
}

#[cfg(target_os = "macos")]
async fn read_frame_with_timeout<T: serde::de::DeserializeOwned>(
    stream: &mut tokio::net::UnixStream,
    timeout: Duration,
) -> Result<T, AppAndroidOwnerBootstrapError> {
    use tokio::io::AsyncReadExt as _;

    let mut length = [0_u8; 4];
    tokio::time::timeout(timeout, stream.read_exact(&mut length))
        .await
        .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?
        .map_err(|error| AppAndroidOwnerBootstrapError::Io(error.to_string()))?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)?;
    if length == 0 || length > APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES {
        return Err(AppAndroidOwnerBootstrapError::InvalidFrame);
    }
    let mut bytes = vec![0_u8; length];
    tokio::time::timeout(timeout, stream.read_exact(&mut bytes))
        .await
        .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?
        .map_err(|error| AppAndroidOwnerBootstrapError::Io(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|_| AppAndroidOwnerBootstrapError::InvalidFrame)
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    parent_device: u64,
    parent_inode: u64,
    socket_device: u64,
    socket_inode: u64,
}

#[cfg(target_os = "macos")]
fn validate_private_socket_path(
    socket_path: &std::path::Path,
) -> Result<SocketIdentity, AppAndroidOwnerBootstrapError> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let parent = socket_path
        .parent()
        .ok_or(AppAndroidOwnerBootstrapError::Unavailable)?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?;
    let socket_metadata = std::fs::symlink_metadata(socket_path)
        .map_err(|_| AppAndroidOwnerBootstrapError::Unavailable)?;
    let uid = unsafe { libc::geteuid() };
    if std::fs::canonicalize(parent).ok().as_deref() != Some(parent)
        || parent_metadata.file_type().is_symlink()
        || !parent_metadata.is_dir()
        || parent_metadata.uid() != uid
        || parent_metadata.mode() & 0o777 != 0o700
        || socket_metadata.file_type().is_symlink()
        || !socket_metadata.file_type().is_socket()
        || socket_metadata.uid() != uid
        || socket_metadata.mode() & 0o777 != 0o600
        || socket_metadata.nlink() != 1
    {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    Ok(SocketIdentity {
        parent_device: parent_metadata.dev(),
        parent_inode: parent_metadata.ino(),
        socket_device: socket_metadata.dev(),
        socket_inode: socket_metadata.ino(),
    })
}

#[cfg(target_os = "macos")]
async fn verify_connected_peer(
    stream: &tokio::net::UnixStream,
    socket_path: PathBuf,
    expected_socket: SocketIdentity,
) -> Result<(), AppAndroidOwnerBootstrapError> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let duplicated = unsafe { libc::dup(stream.as_raw_fd()) };
    if duplicated < 0 {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let descriptor = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicated) };
    tokio::task::spawn_blocking(move || verify_peer_code(descriptor, &socket_path, expected_socket))
        .await
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?
}

#[cfg(target_os = "macos")]
fn verify_peer_code(
    descriptor: std::os::fd::OwnedFd,
    socket_path: &std::path::Path,
    expected_socket: SocketIdentity,
) -> Result<(), AppAndroidOwnerBootstrapError> {
    use std::{os::fd::AsRawFd as _, str::FromStr as _};

    use core_foundation::{base::TCFType as _, data::CFData};
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement, SecStaticCode,
    };

    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    if unsafe { libc::getpeereid(descriptor.as_raw_fd(), &mut peer_uid, &mut peer_gid) } != 0
        || peer_uid != unsafe { libc::geteuid() }
    {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let mut peer_pid: libc::pid_t = 0;
    let mut peer_pid_length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut peer_pid as *mut libc::pid_t).cast(),
            &mut peer_pid_length,
        )
    } != 0
        || peer_pid <= 1
        || peer_pid_length as usize != std::mem::size_of::<libc::pid_t>()
    {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let mut peer_token = [0_u8; 32];
    let mut peer_token_length = peer_token.len() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            peer_token.as_mut_ptr().cast(),
            &mut peer_token_length,
        )
    } != 0
        || peer_token_length as usize != peer_token.len()
    {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let token_word = |index: usize| {
        let offset = index * std::mem::size_of::<u32>();
        u32::from_ne_bytes(
            peer_token[offset..offset + std::mem::size_of::<u32>()]
                .try_into()
                .expect("fixed audit-token word"),
        )
    };
    if token_word(1) != peer_uid || token_word(5) != peer_pid as u32 {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }

    let self_code = SecCode::for_self(Flags::NONE)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;
    let team_id = signing_team_id(&self_code)?;
    if team_id.is_empty()
        || team_id.len() > 32
        || !team_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let requirement_text = format!(
        "identifier \"{}\" and anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
        magician_app_contract::android_owner::APP_ANDROID_OWNER_DESKTOP_BUNDLE_ID,
        team_id,
    );
    let requirement = SecRequirement::from_str(&requirement_text)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;
    // SecCodeCheckValidity validates a live guest and rejects
    // CHECK_NESTED_CODE as an inappropriate flag. Keep the live validation
    // strict, then apply nested-code validation to the desktop's static app
    // bundle below where that flag is defined to operate.
    let live_flags = Flags::STRICT_VALIDATE | Flags::NO_NETWORK_ACCESS;
    let static_flags = Flags::STRICT_VALIDATE | Flags::CHECK_NESTED_CODE | Flags::NO_NETWORK_ACCESS;
    self_code
        .check_validity(
            Flags::NO_NETWORK_ACCESS,
            &SecRequirement::from_str(&format!(
                "anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
                team_id,
            ))
            .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?,
        )
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;

    let mut attributes = GuestAttributes::new();
    let peer_token_data = CFData::from_buffer(&peer_token);
    attributes.set_audit_token(peer_token_data.as_concrete_TypeRef());
    let guest = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;
    guest
        .check_validity(live_flags, &requirement)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;
    if signing_team_id(&guest)? != team_id {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let guest_url = guest
        .path(Flags::NONE)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;
    let static_code = SecStaticCode::from_path(&guest_url, Flags::NONE)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;
    static_code
        .check_validity(static_flags, &requirement)
        .map_err(|_| AppAndroidOwnerBootstrapError::PeerIdentityRejected)?;

    // Re-sample the path identity after the potentially expensive code-sign
    // validation so unlink/rebind cannot redirect the accepted connection.
    if validate_private_socket_path(socket_path)? != expected_socket {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let _ = peer_gid;
    Ok(())
}

#[cfg(target_os = "macos")]
fn signing_team_id(
    code: &security_framework::os::macos::code_signing::SecCode,
) -> Result<String, AppAndroidOwnerBootstrapError> {
    use core_foundation::{base::TCFType as _, string::CFString};
    use core_foundation_sys::{
        base::{CFGetTypeID, CFRelease, CFTypeRef},
        dictionary::{CFDictionaryGetValue, CFDictionaryRef},
        string::{CFStringGetTypeID, CFStringRef},
    };

    const SIGNING_INFORMATION: u32 = 1 << 1;
    extern "C" {
        static kSecCodeInfoTeamIdentifier: CFStringRef;
        fn SecCodeCopySigningInformation(
            code: security_framework_sys::code_signing::SecCodeRef,
            flags: u32,
            information: *mut CFDictionaryRef,
        ) -> i32;
    }

    let mut information: CFDictionaryRef = std::ptr::null();
    let status = unsafe {
        SecCodeCopySigningInformation(
            code.as_concrete_TypeRef(),
            SIGNING_INFORMATION,
            &mut information,
        )
    };
    if status != 0 || information.is_null() {
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let value = unsafe {
        CFDictionaryGetValue(
            information,
            kSecCodeInfoTeamIdentifier.cast::<std::ffi::c_void>(),
        )
    };
    if value.is_null()
        || unsafe { CFGetTypeID(value as CFTypeRef) } != unsafe { CFStringGetTypeID() }
    {
        unsafe { CFRelease(information as CFTypeRef) };
        return Err(AppAndroidOwnerBootstrapError::PeerIdentityRejected);
    }
    let team = unsafe { CFString::wrap_under_get_rule(value as CFStringRef) }.to_string();
    unsafe { CFRelease(information as CFTypeRef) };
    Ok(team)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_socket_uses_the_stable_bounded_support_path() {
        let path = desktop_bootstrap_socket_path().expect("desktop bootstrap socket path");
        assert!(path.ends_with(
            std::path::Path::new(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_SUPPORT_DIRECTORY)
                .join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY)
                .join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME),
        ));
        #[cfg(target_os = "macos")]
        {
            let address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
            assert!(path.as_os_str().as_encoded_bytes().len() < address.sun_path.len());
        }
    }

    #[test]
    fn fresh_nonce_is_minted_only_after_peer_code_verification() {
        let source = include_str!("android_owner_bootstrap.rs");
        let run_once = source
            .split_once("pub async fn run_once(")
            .expect("macOS bootstrap entrypoint")
            .1
            .split_once("#[cfg(not(target_os = \"macos\"))]")
            .expect("macOS bootstrap boundary")
            .0;
        let peer_verification = run_once
            .find("verify_connected_peer(&stream")
            .expect("reciprocal peer verification");
        let retained_nonce = run_once
            .find(".bootstrap_hello(Utc::now().timestamp_millis())")
            .expect("retained nonce point-of-use check");
        let readiness = run_once
            .find("AppAndroidOwnerBootstrapDesktopMessage::PeerVerified")
            .expect("verified-peer readiness barrier");
        let fresh_nonce = run_once
            .find("None => store.begin_bootstrap(Utc::now().timestamp_millis())")
            .expect("fresh nonce mint");
        assert!(peer_verification < readiness);
        assert!(readiness < retained_nonce);
        assert!(retained_nonce < fresh_nonce);
    }
}
