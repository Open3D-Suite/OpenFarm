// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeping a secret on disk with the Windows Data Protection API: Windows encrypts it with a key
//! derived from the user's sign-in, so only programs running as the same user on the same computer
//! can read it back. A copy of the file is useless anywhere else.

use std::ffi::c_void;

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// Mixed into the encryption, so a blob made for something else is not taken for an OpenFarm key.
const PURPOSE: &[u8] = b"OpenFarm machine key";

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 }
}

/// Copies what Windows returned and gives its memory back, wiping it first.
///
/// SAFETY: `out` must have been filled by CryptProtectData or CryptUnprotectData.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    let bytes = std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize);
    let copy = bytes.to_vec();
    bytes.fill(0);
    let _ = LocalFree(Some(HLOCAL(out.pbData as *mut c_void)));
    copy
}

/// Encrypts `secret` for the current Windows user.
pub fn protect(secret: &[u8]) -> Result<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the input blobs point at slices that outlive the call and are only read; `out` is
    // filled by Windows and released in `take`.
    unsafe {
        CryptProtectData(
            &blob(secret),
            PCWSTR::null(),
            Some(&blob(PURPOSE)),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .context("Windows could not protect this machine's key")?;
        Ok(take(out))
    }
}

/// Decrypts what `protect` made. Fails for another user, another computer or a damaged blob.
pub fn unprotect(protected: &[u8]) -> Result<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        CryptUnprotectData(
            &blob(protected),
            None,
            Some(&blob(PURPOSE)),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .context("Windows could not read this machine's key (it was protected by another user or on another computer)")?;
        Ok(take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protected_secret_comes_back_and_is_not_readable_on_disk() {
        let secret = b"thirty-two bytes of a secret key";
        let protected = protect(secret).unwrap();
        assert!(protected.len() > secret.len());
        assert!(!protected.windows(secret.len()).any(|w| w == secret));
        assert_eq!(unprotect(&protected).unwrap(), secret);
        // Protecting twice gives different blobs: nothing can be told from comparing them.
        assert_ne!(protect(secret).unwrap(), protected);
    }

    #[test]
    fn a_damaged_or_foreign_blob_is_refused() {
        let mut protected = protect(b"secret").unwrap();
        let last = protected.len() - 1;
        protected[last] ^= 0xff;
        assert!(unprotect(&protected).is_err());
        assert!(unprotect(b"not a blob").is_err());
        assert!(unprotect(b"").is_err());
    }
}
