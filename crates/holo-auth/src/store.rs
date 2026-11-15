use std::path::PathBuf;

use crate::{AuthError, AuthResult, TokenSet};

/// Persistent token storage keyed by account id.
pub trait TokenStore: Send + Sync {
    fn load(&self, key: &str) -> AuthResult<Option<TokenSet>>;
    fn save(&self, key: &str, tokens: &TokenSet) -> AuthResult<()>;
    fn delete(&self, key: &str) -> AuthResult<()>;
}

/// One file per account. On Windows the content is encrypted with DPAPI
/// (`CryptProtectData`, current-user scope), so only this Windows user can read it.
pub struct FileTokenStore {
    dir: PathBuf,
}

impl FileTokenStore {
    pub fn new(dir: impl Into<PathBuf>) -> AuthResult<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.token"))
    }
}

impl TokenStore for FileTokenStore {
    fn load(&self, key: &str) -> AuthResult<Option<TokenSet>> {
        let path = self.path(key);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let plain = protect::unprotect(&data)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|e| AuthError::Storage(e.to_string()))
    }

    fn save(&self, key: &str, tokens: &TokenSet) -> AuthResult<()> {
        let plain = serde_json::to_vec(tokens).map_err(|e| AuthError::Storage(e.to_string()))?;
        let data = protect::protect(&plain)?;
        let path = self.path(key);
        let tmp = path.with_extension("token.tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn delete(&self, key: &str) -> AuthResult<()> {
        match std::fs::remove_file(self.path(key)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(windows)]
mod protect {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };

    use crate::{AuthError, AuthResult};

    const ENTROPY: &[u8] = b"holo-files/token/v1";

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI allocated `out` with LocalAlloc; we copy then free it once.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
            v
        }
    }

    pub fn protect(plain: &[u8]) -> AuthResult<Vec<u8>> {
        let input = blob(plain);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input/entropy point at live slices for the call's duration.
        unsafe {
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| AuthError::Storage(format!("CryptProtectData: {e}")))?;
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> AuthResult<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as above.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| AuthError::Storage(format!("CryptUnprotectData: {e}")))?;
        Ok(take(out))
    }
}

#[cfg(not(windows))]
mod protect {
    // Non-Windows builds exist only for CI / tests; tokens are stored unencrypted.
    use crate::AuthResult;
    pub fn protect(plain: &[u8]) -> AuthResult<Vec<u8>> {
        Ok(plain.to_vec())
    }
    pub fn unprotect(data: &[u8]) -> AuthResult<Vec<u8>> {
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypted() {
        let dir = std::env::temp_dir().join(format!("holo-auth-test-{}", std::process::id()));
        let store = FileTokenStore::new(&dir).unwrap();
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: chrono::Utc::now(),
            scope: None,
        };
        store.save("acc:1", &t).unwrap();
        let raw = std::fs::read(dir.join("acc_1.token")).unwrap();
        #[cfg(windows)]
        assert!(!String::from_utf8_lossy(&raw).contains("\"access_token\""));
        let back = store.load("acc:1").unwrap().unwrap();
        assert_eq!(back.refresh_token.as_deref(), Some("r"));
        store.delete("acc:1").unwrap();
        assert!(store.load("acc:1").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
