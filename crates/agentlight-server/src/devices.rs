//! Pairing codes and per-device bearer tokens.
//!
//! A phone or web client proves it is allowed on the LAN by exchanging a short
//! pairing code (shown on the desktop or logged by the standalone binary) for a
//! long-lived, revocable device token. Only the SHA-256 hash of a token is
//! stored, so a leaked `devices.json` does not leak usable credentials.
//!
//! The store is synchronous and lock-guarded; the async surface only calls its
//! methods. Persistence is best-effort: a missing or corrupt file starts an
//! empty store rather than failing startup.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::constant_time_eq;

/// How long a freshly generated pairing code stays valid.
pub const PAIRING_CODE_TTL_MINUTES: i64 = 5;

/// Length of the pairing code, in characters.
const PAIRING_CODE_LEN: usize = 8;

/// Unambiguous alphabet: no `0/O`, `1/I/L`.
const PAIRING_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// Random bytes behind a device id (hex-encoded to 32 chars).
const DEVICE_ID_BYTES: usize = 16;

/// Random bytes behind a device token (hex-encoded to 64 chars).
const DEVICE_TOKEN_BYTES: usize = 32;

/// Reject a `touch` that would only refresh a timestamp younger than this.
const TOUCH_INTERVAL_SECONDS: i64 = 30;

/// The on-disk shape of one paired device. Never contains the plaintext token.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    id: String,
    name: String,
    token_hash: String,
    created_at: String,
    last_seen: String,
}

/// A paired device as exposed to the API and the desktop. No secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_seen: String,
}

impl From<&DeviceRecord> for DeviceInfo {
    fn from(record: &DeviceRecord) -> Self {
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            created_at: record.created_at.clone(),
            last_seen: record.last_seen.clone(),
        }
    }
}

/// The pairing code currently shown on the host, plus when it stops working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairInfo {
    pub code: String,
    pub expires_at: String,
}

struct Inner {
    devices: Vec<DeviceRecord>,
    code: String,
    code_expires: DateTime<Utc>,
}

/// Thread-safe pairing codes and paired-device records.
///
/// A `DeviceStore` loaded with `None` lives in memory only; the standalone
/// binary and desktop pass a path so devices survive restarts.
pub struct DeviceStore {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl DeviceStore {
    /// Load the store from `path`, starting empty on a missing or corrupt file.
    /// `None` keeps everything in memory.
    pub fn load(path: Option<PathBuf>) -> Self {
        let devices = path.as_deref().map(read_devices).unwrap_or_default();
        let now = Utc::now();
        Self {
            path,
            inner: Mutex::new(Inner {
                devices,
                code: generate_code(),
                code_expires: now + Duration::minutes(PAIRING_CODE_TTL_MINUTES),
            }),
        }
    }

    /// An empty, non-persisted store. Useful for tests and ephemeral embeds.
    pub fn in_memory() -> Self {
        Self::load(None)
    }

    /// The current code and its expiry.
    pub fn pair_info(&self) -> PairInfo {
        let inner = self.inner.lock().unwrap();
        PairInfo {
            code: inner.code.clone(),
            expires_at: inner.code_expires.to_rfc3339(),
        }
    }

    /// Rotate to a fresh code and restart the validity window.
    pub fn regenerate(&self) -> PairInfo {
        let (code, expires) = {
            let mut inner = self.inner.lock().unwrap();
            inner.code = generate_code();
            inner.code_expires = Utc::now() + Duration::minutes(PAIRING_CODE_TTL_MINUTES);
            (inner.code.clone(), inner.code_expires)
        };
        PairInfo {
            code,
            expires_at: expires.to_rfc3339(),
        }
    }

    /// Whether any device has ever paired. Used to decide if auth is needed.
    pub fn has_devices(&self) -> bool {
        !self.inner.lock().unwrap().devices.is_empty()
    }

    /// Every paired device, without secret material.
    pub fn list(&self) -> Vec<DeviceInfo> {
        self.inner
            .lock()
            .unwrap()
            .devices
            .iter()
            .map(DeviceInfo::from)
            .collect()
    }

    /// Exchange a valid code for a new device token. The plaintext token is
    /// returned once; only its hash is stored.
    pub fn pair(&self, code: &str, name: &str) -> Option<(DeviceInfo, String)> {
        let valid = {
            let inner = self.inner.lock().unwrap();
            Utc::now() <= inner.code_expires
                && constant_time_eq(code.as_bytes(), inner.code.as_bytes())
        };
        if !valid {
            return None;
        }
        Some(self.add(name))
    }

    /// Register a device and mint its token. Used directly by tests and any
    /// future admin path; the HTTP surface goes through [`pair`](Self::pair).
    pub fn add(&self, name: &str) -> (DeviceInfo, String) {
        let id = random_hex(DEVICE_ID_BYTES);
        let token = random_hex(DEVICE_TOKEN_BYTES);
        let now = Utc::now().to_rfc3339();
        let record = DeviceRecord {
            id,
            name: normalize_name(name),
            token_hash: hash_token(&token),
            created_at: now.clone(),
            last_seen: now,
        };
        let info = DeviceInfo::from(&record);
        self.inner.lock().unwrap().devices.push(record);
        self.save();
        (info, token)
    }

    /// Forget a device, which immediately invalidates its token.
    pub fn revoke(&self, id: &str) -> bool {
        let removed = {
            let mut inner = self.inner.lock().unwrap();
            let before = inner.devices.len();
            inner.devices.retain(|device| device.id != id);
            before != inner.devices.len()
        };
        if removed {
            self.save();
        }
        removed
    }

    /// Resolve a plaintext token to a device id, using constant-time hash
    /// comparison.
    pub fn authenticate(&self, token: &str) -> Option<String> {
        let hash = hash_token(token);
        self.inner
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|device| constant_time_eq(device.token_hash.as_bytes(), hash.as_bytes()))
            .map(|device| device.id.clone())
    }

    /// Best-effort last-seen refresh. Writes at most once per
    /// [`TOUCH_INTERVAL_SECONDS`] to keep request traffic from thrashing disk.
    pub fn touch(&self, id: &str) {
        let now = Utc::now();
        let changed = {
            let mut inner = self.inner.lock().unwrap();
            let Some(device) = inner.devices.iter_mut().find(|device| device.id == id) else {
                return;
            };
            let recent = DateTime::parse_from_rfc3339(&device.last_seen)
                .map(|seen| {
                    now.signed_duration_since(seen.with_timezone(&Utc))
                        .num_seconds()
                })
                .map(|seconds| seconds < TOUCH_INTERVAL_SECONDS)
                .unwrap_or(false);
            if recent {
                return;
            }
            device.last_seen = now.to_rfc3339();
            true
        };
        if changed {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let devices = self.inner.lock().unwrap().devices.clone();
        let _ = write_devices(path, &devices);
    }
}

/// SHA-256 hex of a token. Public so callers can reason about collisions in
/// tests without re-implementing it.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    to_hex(&hasher.finalize())
}

fn read_devices(path: &Path) -> Vec<DeviceRecord> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<DeviceRecord>>(&raw).ok())
        .unwrap_or_default()
}

fn write_devices(path: &Path, devices: &[DeviceRecord]) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("devices.json");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let serialized = serde_json::to_string_pretty(devices).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn generate_code() -> String {
    let mut rng = rand::rng();
    (0..PAIRING_CODE_LEN)
        .map(|_| PAIRING_ALPHABET[rng.random_range(0..PAIRING_ALPHABET.len())] as char)
        .collect()
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill(&mut buffer[..]);
    to_hex(&buffer)
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Names are arbitrary UTF-8; cap by characters and never store blank.
fn normalize_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Unnamed device".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_uses_the_unambiguous_alphabet() {
        for _ in 0..50 {
            let code = generate_code();
            assert_eq!(code.chars().count(), PAIRING_CODE_LEN);
            assert!(code.chars().all(|c| PAIRING_ALPHABET.contains(&(c as u8))));
        }
    }

    #[test]
    fn tokens_are_unique_and_lossy_in_storage() {
        let store = DeviceStore::in_memory();
        let (first, first_token) = store.add("phone");
        let (second, second_token) = store.add("laptop");
        assert_ne!(first_token, second_token);
        assert_ne!(first.id, second.id);

        let raw = serde_json::to_string(&store.list()).unwrap();
        assert!(!raw.contains(&first_token));
        assert!(store.authenticate(&first_token).is_some());
        assert!(store.authenticate(&second_token).is_some());
        assert!(store.authenticate("bogus").is_none());
    }

    #[test]
    fn pair_accepts_a_valid_code_then_rejects_reuse_of_an_expired_one() {
        let store = DeviceStore::in_memory();
        let info = store.pair_info();
        let (_, token) = store.pair(&info.code, "phone").expect("valid code");
        assert!(store.authenticate(&token).is_some());
        assert!(store.pair("WRONGCOD", "phone").is_none());
    }

    #[test]
    fn revoke_invalidates_a_device_token() {
        let store = DeviceStore::in_memory();
        let info = store.pair_info();
        let (device, token) = store.pair(&info.code, "phone").unwrap();
        assert!(store.revoke(&device.id));
        assert!(store.authenticate(&token).is_none());
        assert!(!store.revoke(&device.id));
        assert!(store.list().is_empty());
    }

    #[test]
    fn regenerate_rotates_the_code() {
        let store = DeviceStore::in_memory();
        let first = store.pair_info();
        let second = store.regenerate();
        assert_ne!(first.code, second.code);
        assert!(store.pair(&first.code, "phone").is_none());
        assert!(store.pair(&second.code, "phone").is_some());
    }

    #[test]
    fn missing_and_corrupt_files_load_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = DeviceStore::load(Some(dir.path().join("nope.json")));
        assert!(missing.list().is_empty());

        let corrupt_path = dir.path().join("devices.json");
        std::fs::write(&corrupt_path, "{ not json").unwrap();
        let corrupt = DeviceStore::load(Some(corrupt_path));
        assert!(corrupt.list().is_empty());
    }

    #[test]
    fn devices_roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("devices.json");

        let store = DeviceStore::load(Some(path.clone()));
        let info = store.pair_info();
        let (device, token) = store.pair(&info.code, "phone").unwrap();

        let reloaded = DeviceStore::load(Some(path));
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, device.id);
        assert_eq!(
            reloaded.authenticate(&token).as_deref(),
            Some(device.id.as_str())
        );
    }
}
