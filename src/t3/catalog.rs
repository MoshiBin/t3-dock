//! The remote environments that the T3 Code desktop client has saved.
//!
//! The client keeps them in `userdata/connection-catalog.json`, encrypted with Electron's
//! `safeStorage`. On Windows that is Chromium's `os_crypt`: an AES-256-GCM key in the client's
//! `Local State`, itself sealed with DPAPI for the current user. The catalog also holds the
//! client's own credentials for each environment. Those are never deserialized or used: only the
//! SSH targets are read, and the decrypted text is wiped once parsed.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use crate::win;

/// Where the desktop client keeps its Electron user data, newest first. Mirrors
/// `resolveUserDataPath` in the desktop bundle.
const USER_DATA_DIRS: [&str; 3] = ["t3code-v2", "T3 Code (Alpha)", "t3code"];
const DPAPI_PREFIX: &[u8] = b"DPAPI";
const VERSION_PREFIX: &[u8] = b"v10";
const NONCE_LEN: usize = 12;

/// How to reach a remote environment over SSH, as the desktop client saved it.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct SshTarget {
    pub alias: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: Option<u16>,
}

/// A remote environment. No `Debug`: the id and host are private.
#[derive(Clone, PartialEq, Eq)]
pub struct Remote {
    pub environment_id: String,
    pub label: String,
    pub ssh: SshTarget,
}

/// What a catalog read found.
#[derive(Default)]
pub struct Catalog {
    pub remotes: Vec<Remote>,
    /// Saved environments that are not reached over SSH, which t3-dock does not support.
    pub unsupported: usize,
}

#[derive(Debug)]
pub enum CatalogError {
    /// No catalog file, which means no remote environments.
    Missing,
    Unreadable,
    NoKey,
    Undecryptable,
    Malformed,
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "T3 has no saved remote environments",
            Self::Unreadable => "cannot read T3's connection catalog",
            Self::NoKey => "cannot unseal the T3 client's storage key",
            Self::Undecryptable => "cannot decrypt T3's connection catalog",
            Self::Malformed => "T3's connection catalog has an unexpected shape",
        })
    }
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "encryptedCatalog")]
    encrypted_catalog: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    targets: Vec<TargetEntry>,
    profiles: Vec<ProfileEntry>,
    #[serde(default)]
    disabled_environment_ids: Vec<String>,
    // `credentials` and `remoteDpopTokens` are left out on purpose.
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetEntry {
    #[serde(rename = "_tag")]
    tag: String,
    environment_id: String,
    label: Option<String>,
    connection_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileEntry {
    #[serde(rename = "_tag")]
    tag: String,
    connection_id: String,
    target: Option<SshTarget>,
}

pub fn catalog_path(t3_home: &Path) -> PathBuf {
    t3_home.join("userdata").join("connection-catalog.json")
}

/// Reads the client's saved remote environments.
pub fn load(t3_home: &Path) -> Result<Catalog, CatalogError> {
    let text = match std::fs::read_to_string(catalog_path(t3_home)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CatalogError::Missing);
        }
        Err(_) => return Err(CatalogError::Unreadable),
    };
    let appdata = std::env::var_os("APPDATA").ok_or(CatalogError::NoKey)?;
    let key = storage_key(Path::new(&appdata)).ok_or(CatalogError::NoKey)?;
    let mut plain = decrypt(&key, &text)?;
    let parsed = parse(&plain);
    plain.zeroize();
    parsed
}

/// The client's `os_crypt` key, from the first user data folder that has one.
fn storage_key(appdata: &Path) -> Option<Zeroizing<Vec<u8>>> {
    #[derive(Deserialize)]
    struct LocalState {
        os_crypt: OsCrypt,
    }
    #[derive(Deserialize)]
    struct OsCrypt {
        encrypted_key: String,
    }
    let dir = USER_DATA_DIRS
        .iter()
        .map(|name| appdata.join(name).join("Local State"))
        .find(|path| path.is_file())?;
    let state: LocalState = serde_json::from_str(&std::fs::read_to_string(dir).ok()?).ok()?;
    let sealed = STANDARD.decode(state.os_crypt.encrypted_key).ok()?;
    let sealed = sealed.strip_prefix(DPAPI_PREFIX)?;
    let key = Zeroizing::new(win::unprotect(sealed)?);
    (key.len() == 32).then_some(key)
}

fn decrypt(key: &[u8], envelope: &str) -> Result<Zeroizing<Vec<u8>>, CatalogError> {
    let envelope: Envelope = serde_json::from_str(envelope).map_err(|_| CatalogError::Malformed)?;
    let blob = STANDARD
        .decode(envelope.encrypted_catalog)
        .map_err(|_| CatalogError::Malformed)?;
    let body = blob
        .strip_prefix(VERSION_PREFIX)
        .filter(|body| body.len() > NONCE_LEN)
        .ok_or(CatalogError::Undecryptable)?;
    let (nonce, sealed) = body.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CatalogError::NoKey)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map(Zeroizing::new)
        .map_err(|_| CatalogError::Undecryptable)
}

fn parse(plain: &[u8]) -> Result<Catalog, CatalogError> {
    let document: Document = serde_json::from_slice(plain).map_err(|_| CatalogError::Malformed)?;
    let mut catalog = Catalog::default();
    for target in document.targets {
        if document
            .disabled_environment_ids
            .contains(&target.environment_id)
        {
            continue;
        }
        let ssh = (target.tag == "SshConnectionTarget")
            .then(|| {
                document.profiles.iter().find(|profile| {
                    profile.tag == "SshConnectionProfile"
                        && Some(&profile.connection_id) == target.connection_id.as_ref()
                })
            })
            .flatten()
            .and_then(|profile| profile.target.clone())
            .filter(valid_target);
        match ssh {
            Some(ssh) if valid_environment_id(&target.environment_id) => {
                catalog.remotes.push(Remote {
                    label: target.label.unwrap_or_default(),
                    environment_id: target.environment_id,
                    ssh,
                });
            }
            _ => catalog.unsupported += 1,
        }
    }
    Ok(catalog)
}

/// Ids name token files and go into deep links, so only plain ones are accepted.
pub fn valid_environment_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The parts become `ssh.exe` arguments, so nothing that could read as an option or break a word.
fn valid_target(target: &SshTarget) -> bool {
    let word = |value: &str| {
        !value.starts_with('-')
            && value
                .bytes()
                .all(|b| b.is_ascii_graphic() && !b"\"'`$;&|<>\\".contains(&b))
    };
    let destination = if target.alias.trim().is_empty() {
        &target.hostname
    } else {
        &target.alias
    };
    !destination.trim().is_empty()
        && word(destination.trim())
        && target
            .username
            .as_deref()
            .is_none_or(|user| !user.is_empty() && word(user) && !user.contains('@'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn seal(key: &[u8], plain: &str) -> String {
        let nonce = [3u8; NONCE_LEN];
        let cipher = Aes256Gcm::new_from_slice(key).unwrap();
        let sealed = cipher
            .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
            .unwrap();
        let blob = [VERSION_PREFIX, &nonce, &sealed].concat();
        json!({"version": 1, "encryptedCatalog": STANDARD.encode(blob)}).to_string()
    }

    fn document() -> serde_json::Value {
        json!({
            "schemaVersion": 1,
            "targets": [
                {"_tag": "SshConnectionTarget", "environmentId": "env-a", "label": "Box A",
                 "connectionId": "ssh:a"},
                {"_tag": "SshConnectionTarget", "environmentId": "env-off", "label": "Off",
                 "connectionId": "ssh:off"},
                {"_tag": "BearerConnectionTarget", "environmentId": "env-b", "label": "B",
                 "connectionId": "bearer:b"},
                {"_tag": "RelayConnectionTarget", "environmentId": "env-c", "label": "C"}
            ],
            "profiles": [
                {"_tag": "SshConnectionProfile", "connectionId": "ssh:a", "environmentId": "env-a",
                 "target": {"alias": "box-a", "hostname": "box-a.invalid", "username": "me",
                            "port": 22}},
                {"_tag": "SshConnectionProfile", "connectionId": "ssh:off",
                 "environmentId": "env-off",
                 "target": {"alias": "off", "hostname": "off.invalid", "username": null,
                            "port": null}},
                {"_tag": "BearerConnectionProfile", "connectionId": "bearer:b",
                 "httpBaseUrl": "https://b.invalid/", "wsBaseUrl": "wss://b.invalid/"}
            ],
            "credentials": [{"connectionId": "ssh:a",
                             "credential": {"_tag": "BearerConnectionCredential",
                                            "token": "invented-secret"}}],
            "remoteDpopTokens": [],
            "disabledEnvironmentIds": ["env-off"]
        })
    }

    #[test]
    fn reads_ssh_targets_and_skips_the_rest() {
        let key = [7u8; 32];
        let envelope = seal(&key, &document().to_string());
        let plain = decrypt(&key, &envelope).unwrap();
        let catalog = parse(&plain).unwrap();
        assert_eq!(catalog.remotes.len(), 1);
        let remote = &catalog.remotes[0];
        assert_eq!(remote.environment_id, "env-a");
        assert_eq!(remote.label, "Box A");
        assert_eq!(remote.ssh.alias, "box-a");
        assert_eq!(remote.ssh.username.as_deref(), Some("me"));
        assert_eq!(remote.ssh.port, Some(22));
        assert_eq!(catalog.unsupported, 2);
    }

    #[test]
    fn a_wrong_key_or_a_tampered_catalog_is_refused() {
        let envelope = seal(&[7u8; 32], &document().to_string());
        assert!(matches!(
            decrypt(&[8u8; 32], &envelope),
            Err(CatalogError::Undecryptable)
        ));
        assert!(matches!(
            decrypt(&[7u8; 32], "{\"encryptedCatalog\": \"bm90IHNlYWxlZA==\"}"),
            Err(CatalogError::Undecryptable)
        ));
    }

    #[test]
    fn targets_that_could_inject_ssh_options_are_refused() {
        let target = |alias: &str, user: Option<&str>| SshTarget {
            alias: alias.into(),
            hostname: "host.invalid".into(),
            username: user.map(Into::into),
            port: None,
        };
        assert!(valid_target(&target("box", Some("me"))));
        assert!(valid_target(&target("", None)));
        assert!(!valid_target(&target("-oProxyCommand=calc", None)));
        assert!(!valid_target(&target("box name", None)));
        assert!(!valid_target(&target("box", Some("-l"))));
        assert!(!valid_target(&target("box", Some("a@b"))));
        assert!(!valid_target(&target("box;calc", None)));
    }

    #[test]
    fn environment_ids_must_be_plain() {
        assert!(valid_environment_id("0f3a6c1e-7b2d-4e9a-8c5f-1d2e3f4a5b6c"));
        assert!(!valid_environment_id("../escape"));
        assert!(!valid_environment_id(""));
    }
}
