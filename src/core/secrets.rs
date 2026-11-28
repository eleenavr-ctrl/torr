use crate::core::error::{CoreError, CoreResult};
use crate::platform::windows::dpapi;
use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub trait SecretStore: Send + Sync {
    fn get_secret(&self, key: &str) -> CoreResult<Option<String>>;
    fn set_secret(&mut self, key: &str, value: &str) -> CoreResult<()>;
    fn remove_secret(&mut self, key: &str) -> CoreResult<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct EncryptedSecretsEnvelope {
    pub mode: String, // "dpapi" | "argon2-chacha20"
    pub salt: Vec<u8>,
    pub nonce: Vec<u8>,
    pub payload: Vec<u8>,
}

pub struct FileSecretStore {
    path: std::path::PathBuf,
    read_only: bool,
    master_password: Option<String>,
    cache: BTreeMap<String, String>,
}

impl FileSecretStore {
    pub fn open(path: &Path, read_only: bool, master_password: Option<String>) -> CoreResult<Self> {
        let mut store = Self {
            path: path.to_path_buf(),
            read_only,
            master_password,
            cache: BTreeMap::new(),
        };
        if path.exists() {
            store.reload()?;
        }
        Ok(store)
    }

    pub fn set_master_password(&mut self, pw: Option<String>) {
        self.master_password = pw;
    }

    pub fn reload(&mut self) -> CoreResult<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let raw = fs::read_to_string(&self.path)?;
        let env: EncryptedSecretsEnvelope = serde_json::from_str(&raw)?;
        let plaintext = match env.mode.as_str() {
            "dpapi" => dpapi::unprotect_bytes(&env.payload).map_err(CoreError::Crypto)?,
            "argon2-chacha20" => {
                let pw = self
                    .master_password
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| CoreError::Crypto("Требуется мастер-пароль".into()))?;
                decrypt_with_password(pw, &env.salt, &env.nonce, &env.payload)?
            }
            other => return Err(CoreError::Crypto(format!("Неизвестный режим: {other}"))),
        };
        self.cache = serde_json::from_slice(&plaintext)?;
        Ok(())
    }

    fn flush(&self) -> CoreResult<()> {
        if self.read_only {
            return Err(CoreError::ReadOnly);
        }
        let plaintext = serde_json::to_vec(&self.cache)?;
        let env = if let Some(pw) = self.master_password.as_deref().filter(|p| !p.is_empty()) {
            let (salt, nonce, payload) = encrypt_with_password(pw, &plaintext)?;
            EncryptedSecretsEnvelope {
                mode: "argon2-chacha20".into(),
                salt,
                nonce,
                payload,
            }
        } else {
            let payload = dpapi::protect_bytes(&plaintext).map_err(CoreError::Crypto)?;
            EncryptedSecretsEnvelope {
                mode: "dpapi".into(),
                salt: vec![],
                nonce: vec![],
                payload,
            }
        };
        let tmp = self.path.with_extension("enc.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&env)?)?;
        if self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn get_secret(&self, key: &str) -> CoreResult<Option<String>> {
        Ok(self.cache.get(key).cloned())
    }

    fn set_secret(&mut self, key: &str, value: &str) -> CoreResult<()> {
        if value.is_empty() {
            self.cache.remove(key);
        } else {
            self.cache.insert(key.to_string(), value.to_string());
        }
        self.flush()
    }

    fn remove_secret(&mut self, key: &str) -> CoreResult<()> {
        self.cache.remove(key);
        self.flush()
    }
}

fn derive_key(password: &str, salt: &[u8]) -> CoreResult<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| CoreError::Crypto(format!("Argon2 error: {e}")))?;
    Ok(key)
}

pub fn encrypt_with_password(password: &str, plain: &[u8]) -> CoreResult<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    use chacha20poly1305::aead::rand_core::RngCore;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(password, &salt)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plain)
        .map_err(|e| CoreError::Crypto(format!("ChaCha20 encrypt error: {e}")))?;
    Ok((salt.to_vec(), nonce.to_vec(), ct))
}

pub fn decrypt_with_password(
    password: &str,
    salt: &[u8],
    nonce_bytes: &[u8],
    ciphertext: &[u8],
) -> CoreResult<Vec<u8>> {
    if nonce_bytes.len() != 12 {
        return Err(CoreError::Crypto("Некорректный nonce".into()));
    }
    let key = derive_key(password, salt)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CoreError::Crypto("Неверный мастер-пароль или повреждены секреты".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_password_roundtrip() {
        let base = std::env::temp_dir().join(format!("ts_sec_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let path = base.join("secrets.enc");

        {
            let mut store = FileSecretStore::open(&path, false, Some("s3cr3t!".into())).unwrap();
            store.set_secret("webdav_pass", "my_token_123").unwrap();
        }
        let store2 = FileSecretStore::open(&path, false, Some("s3cr3t!".into())).unwrap();
        assert_eq!(
            store2.get_secret("webdav_pass").unwrap().as_deref(),
            Some("my_token_123")
        );
        assert!(FileSecretStore::open(&path, false, Some("wrong".into())).is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
