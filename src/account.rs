//! Xbox 360 Profile Account file parser.
//!
//! The Account file is a 404-byte encrypted block stored inside Xbox 360
//! profile packages. It contains gamertag, XUID, country, language,
//! subscription, and other profile data.
//!
//! Encryption (per Free60 `Formats/Profile_Account.md` and arkong.org
//! Xbox 360 File Specifications Reference):
//! - RC4 + HMAC-SHA1 encrypted
//! - RC4 key = first 16 bytes of HMAC-SHA1( first 16 bytes of file
//!   encrypted with key E1BC159C73B1EAE9AB3170F3AD47EBF3 )
//! - Remaining bytes (16 to end) encrypted with derived RC4 key
//!
//! OnlineKey and passport fields are REDACTED by default per the doc's
//! privacy guidance.

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};

type HmacSha1 = Hmac<sha1::Sha1>;

/// Decrypted Xbox 360 Profile Account data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub flags: u32,
    pub live_flags: u32,
    pub gamertag: String,
    pub xuid: u64,
    pub cached_user_flags: u32,
    pub service_provider: String,
    pub passcode: String,
    pub online_domain: String,
    pub kerberos_realm: String,
    pub online_key: String,
    pub passport_membername: String,
    pub passport_password: String,
    pub owner_passport_membername: String,
}

/// RC4 stream cipher implementation.
struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    fn new(key: &[u8]) -> Self {
        let mut s: [u8; 256] = [0; 256];
        for (i, item) in s.iter_mut().enumerate() {
            *item = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j
                .wrapping_add(s[i])
                .wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        Rc4 { s, i: 0, j: 0 }
    }

    fn crypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k = self.s
                [(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize];
            *byte ^= k;
        }
    }
}

/// HMAC-SHA1 implementation using the sha1 crate.
/// HMAC-SHA1 using the hmac crate.
fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut mac = HmacSha1::new_from_slice(key).expect("HMAC key length error");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Decrypt an Xbox 360 Profile Account file (404 bytes).
///
/// The encryption scheme (per Free60 `Formats/Profile_Account.md` and
/// Xenia profile_manager.cc):
/// 1. HMAC-SHA1(key=xekey_0x19[16 bytes], msg=data[0..16]) → 20 bytes
/// 2. First 16 bytes of HMAC = actual RC4 key
/// 3. Remaining bytes (16 to end) decrypted with derived RC4 key
pub fn decrypt_account(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 32 {
        anyhow::bail!("Account file too small ({} bytes, need at least 32)", data.len());
    }

    const BOOTSTRAP_KEY: &[u8] = &[
        0xE1, 0xBC, 0x15, 0x9C, 0x73, 0xB1, 0xEA, 0xE9,
        0xAB, 0x31, 0x70, 0xF3, 0xAD, 0x47, 0xEB, 0xF3,
    ];

    // Step 1: HMAC-SHA1(key=BOOTSTRAP_KEY, msg=first_16_bytes_of_file) → 20 bytes
    let hmac = hmac_sha1(BOOTSTRAP_KEY, &data[..16]);

    // Step 2: First 16 bytes of HMAC = actual RC4 key
    let actual_key = &hmac[..16];

    // Step 3: Decrypt remaining bytes with actual RC4 key
    let mut decrypted = data[16..].to_vec();
    let mut rc4 = Rc4::new(actual_key);
    rc4.crypt(&mut decrypted);

    Ok(decrypted)
}

/// Parse decrypted Account data (404 - 16 = 388 bytes of plaintext).
///
/// The decrypted data starts with an 8-byte confounder (0xFD * 8),
/// followed by the X_XAMACCOUNTINFO struct.
pub fn parse_account(data: &[u8]) -> Result<AccountInfo> {
    if data.len() < 0x17C + 8 {
        anyhow::bail!("Decrypted account data too small ({} bytes)", data.len());
    }

    // Skip 8-byte confounder at start of decrypted data
    let data = &data[8..];

    let be_u32 = |b: &[u8], o: usize| -> Result<u32> {
        Ok(u32::from_be_bytes(
            b.get(o..o + 4).context("short read")?.try_into()?,
        ))
    };
    let be_u64 = |b: &[u8], o: usize| -> Result<u64> {
        Ok(u64::from_be_bytes(
            b.get(o..o + 8).context("short read")?.try_into()?,
        ))
    };

    let flags = be_u32(data, 0x00)?;
    let live_flags = be_u32(data, 0x04)?;

    // Gamertag: 16 chars UTF-16BE at 0x08
    let gamertag = read_utf16be(data, 0x08, 16)?;

    // XUID: u64 BE at 0x18
    let xuid = be_u64(data, 0x18)?;

    // Cached User Flags: u32 BE at 0x20
    let cached_user_flags = be_u32(data, 0x20)?;

    // Service Provider: 4 bytes ASCII at 0x24
    let service_provider = read_ascii(data, 0x24, 4)?;

    // Passcode: 4 bytes at 0x28
    let passcode = read_hex(data, 0x28, 4)?;

    // Online Domain: 20 bytes ASCII at 0x2C
    let online_domain = read_ascii(data, 0x2C, 20)?;

    // Kerberos Realm: 24 bytes ASCII at 0x40
    let kerberos_realm = read_ascii(data, 0x40, 24)?;

    // Online Key: 16 bytes hex at 0x58 — REDACTED by default
    let online_key = read_hex(data, 0x58, 16)?;

    // Passport Membername: 114 bytes ASCII at 0x68 — REDACTED by default
    let passport_membername = read_ascii(data, 0x68, 0x72)?;

    // Passport Password: 32 bytes ASCII at 0xDA — REDACTED by default
    let passport_password = read_ascii(data, 0xDA, 0x20)?;

    // Owner Passport Membername: 114 bytes ASCII at 0xFA — REDACTED by default
    let owner_passport_membername = read_ascii(data, 0xFA, 0x72)?;

    Ok(AccountInfo {
        flags,
        live_flags,
        gamertag,
        xuid,
        cached_user_flags,
        service_provider,
        passcode,
        online_domain,
        kerberos_realm,
        online_key,
        passport_membername,
        passport_password,
        owner_passport_membername,
    })
}

fn read_utf16be(data: &[u8], offset: usize, char_count: usize) -> Result<String> {
    let end = offset + char_count * 2;
    if end > data.len() {
        anyhow::bail!("UTF-16BE string at offset {offset} overruns data");
    }
    let mut units = Vec::new();
    for i in (offset..end).step_by(2) {
        let u = u16::from_be_bytes([data[i], data[i + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
    }
    Ok(String::from_utf16_lossy(&units))
}

fn read_ascii(data: &[u8], offset: usize, byte_count: usize) -> Result<String> {
    let end = offset + byte_count;
    if end > data.len() {
        anyhow::bail!("ASCII string at offset {offset} overruns data");
    }
    let s = String::from_utf8_lossy(&data[offset..end]);
    Ok(s.trim_end_matches('\0').trim().to_string())
}

fn read_hex(data: &[u8], offset: usize, byte_count: usize) -> Result<String> {
    let end = offset + byte_count;
    if end > data.len() {
        anyhow::bail!("Hex data at offset {offset} overruns data");
    }
    Ok(data[offset..end]
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(""))
}

/// Redact sensitive fields from an AccountInfo for safe display.
pub fn redact_account(info: &AccountInfo) -> AccountInfo {
    AccountInfo {
        online_key: "REDACTED".to_string(),
        passport_membername: "REDACTED".to_string(),
        passport_password: "REDACTED".to_string(),
        owner_passport_membername: "REDACTED".to_string(),
        ..info.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rc4_known_vector() {
        // RC4 test vector from RFC 6229
        let key = b"Key";
        let plaintext = b"Plaintext";
        let mut rc4 = Rc4::new(key);
        let mut ciphertext = plaintext.to_vec();
        rc4.crypt(&mut ciphertext);
        let expected: &[u8] = &[0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3];
        assert_eq!(&ciphertext[..9], expected);
    }
}
