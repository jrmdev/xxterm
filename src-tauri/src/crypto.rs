use base64::Engine as _;
use ring::rand::SecureRandom;
use ring::{aead, pbkdf2, rand};
use std::sync::Mutex;

const PBKDF2_ITERATIONS: u32 = 100_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const VERIFICATION_PLAINTEXT: &[u8] = b"xxterm-master-verify";
pub const SETTINGS_MASTER_KEY: &str = "master_password";
pub const SETTINGS_PWD_PREFIX: &str = "encrypted_pwd_";
pub const SETTINGS_KEY_PASSPHRASE_PREFIX: &str = "encrypted_key_passphrase_";
#[allow(dead_code)]
pub const SETTINGS_FONT_FAMILY: &str = "font_family";
#[allow(dead_code)]
pub const SETTINGS_FONT_SIZE: &str = "font_size";

pub struct MasterPasswordState {
    pub key: Mutex<Option<[u8; KEY_LEN]>>,
}

impl MasterPasswordState {
    pub fn new() -> Self {
        Self {
            key: Mutex::new(None),
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.key.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    pub fn lock(&self) -> Result<(), String> {
        let mut guard = self.key.lock().map_err(|e| e.to_string())?;
        *guard = None;
        Ok(())
    }

    fn get_key(&self) -> Result<[u8; KEY_LEN], String> {
        let guard = self.key.lock().map_err(|e| e.to_string())?;
        guard
            .as_ref()
            .copied()
            .ok_or_else(|| "Master password not unlocked".to_string())
    }

    fn set_key(&self, key: [u8; KEY_LEN]) -> Result<(), String> {
        let mut guard = self.key.lock().map_err(|e| e.to_string())?;
        *guard = Some(key);
        Ok(())
    }
}

fn random_bytes(len: usize) -> Result<Vec<u8>, String> {
    let rng = rand::SystemRandom::new();
    let mut buf = vec![0u8; len];
    rng.fill(&mut buf)
        .map_err(|e| format!("RNG error: {}", e))?;
    Ok(buf)
}

fn derive_key(password: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(PBKDF2_ITERATIONS).unwrap(),
        salt,
        password.as_bytes(),
        &mut key,
    );
    key
}

fn encrypt_with_key(plaintext: &[u8], key: &[u8; KEY_LEN]) -> Result<Vec<u8>, String> {
    let nonce = random_bytes(NONCE_LEN)?;
    let mut in_out = plaintext.to_vec();
    let sealing_key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|e| format!("Failed to create sealing key: {}", e))?;
    let sealing_key = aead::LessSafeKey::new(sealing_key);
    let nonce_val = aead::Nonce::assume_unique_for_key(nonce.clone().try_into().unwrap());
    sealing_key
        .seal_in_place_append_tag(nonce_val, aead::Aad::empty(), &mut in_out)
        .map_err(|e| format!("Encryption failed: {}", e))?;

    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&in_out);
    Ok(blob)
}

fn decrypt_with_key(cipher_blob: &[u8], key: &[u8; KEY_LEN]) -> Result<Vec<u8>, String> {
    if cipher_blob.len() < NONCE_LEN + 16 {
        return Err("Invalid cipher data".to_string());
    }
    let nonce = &cipher_blob[..NONCE_LEN];
    let ciphertext = &cipher_blob[NONCE_LEN..];

    let mut in_out = ciphertext.to_vec();
    let opening_key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|e| format!("Failed to create opening key: {}", e))?;
    let opening_key = aead::LessSafeKey::new(opening_key);
    let nonce_val = aead::Nonce::assume_unique_for_key(nonce.to_vec().try_into().unwrap());

    opening_key
        .open_in_place(nonce_val, aead::Aad::empty(), &mut in_out)
        .map_err(|_| "Decryption failed".to_string())?;

    let plain_len = in_out.len() - aead::AES_256_GCM.tag_len();
    in_out.truncate(plain_len);
    Ok(in_out)
}

pub fn has_master_password(store: &crate::session_store::SessionStore) -> Result<bool, String> {
    store.get_setting(SETTINGS_MASTER_KEY).map(|v| v.is_some())
}

pub fn set_master_password(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    password: &str,
) -> Result<(), String> {
    if has_master_password(store)? {
        return Err("Master password is already set".to_string());
    }

    let salt = random_bytes(SALT_LEN)?;
    let key = derive_key(password, &salt);
    let blob = encrypt_with_key(VERIFICATION_PLAINTEXT, &key)?;

    let mut payload = salt.to_vec();
    payload.extend_from_slice(&blob);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);

    store.set_setting(SETTINGS_MASTER_KEY, &encoded)?;
    state.set_key(key)?;

    Ok(())
}

pub fn unlock_master_password(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    password: &str,
) -> Result<(), String> {
    let stored = store
        .get_setting(SETTINGS_MASTER_KEY)?
        .ok_or_else(|| "Master password not set".to_string())?;

    let payload = base64::engine::general_purpose::STANDARD
        .decode(&stored)
        .map_err(|e| format!("Invalid stored data: {}", e))?;

    if payload.len() < SALT_LEN + NONCE_LEN + 16 {
        return Err("Invalid master password data".to_string());
    }

    let salt = &payload[..SALT_LEN];
    let cipher_blob = &payload[SALT_LEN..];

    let key = derive_key(password, salt);

    match decrypt_with_key(cipher_blob, &key) {
        Ok(plaintext) => {
            if plaintext != VERIFICATION_PLAINTEXT {
                return Err("Incorrect master password".to_string());
            }
        }
        Err(_) => return Err("Incorrect master password".to_string()),
    }

    state.set_key(key)?;
    Ok(())
}

pub fn change_master_password(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    old_password: &str,
    new_password: &str,
) -> Result<(), String> {
    unlock_master_password(store, state, old_password)?;

    let encrypted_ids = store.list_encrypted_secret_ids()?;

    let mut secret_map: Vec<(String, i64, String)> = Vec::new();
    for (prefix, id) in &encrypted_ids {
        match get_encrypted_secret_internal(store, state, prefix, *id) {
            Ok(secret) => secret_map.push((prefix.clone(), *id, secret)),
            Err(_) => continue,
        }
    }

    store.delete_setting(SETTINGS_MASTER_KEY)?;

    let new_salt = random_bytes(SALT_LEN)?;
    let new_key = derive_key(new_password, &new_salt);
    let new_blob = encrypt_with_key(VERIFICATION_PLAINTEXT, &new_key)?;

    let mut payload = new_salt.to_vec();
    payload.extend_from_slice(&new_blob);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
    store.set_setting(SETTINGS_MASTER_KEY, &encoded)?;

    for (prefix, session_id, secret) in &secret_map {
        let encrypted = encrypt_with_key(secret.as_bytes(), &new_key)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&encrypted);
        store.set_setting(&format!("{}{}", prefix, session_id), &encoded)?;
    }

    state.set_key(new_key)?;
    Ok(())
}

pub fn encrypt_and_store_password(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    session_id: i64,
    password: &str,
) -> Result<(), String> {
    encrypt_and_store_secret(store, state, SETTINGS_PWD_PREFIX, session_id, password)
}

pub fn encrypt_and_store_key_passphrase(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    session_id: i64,
    passphrase: &str,
) -> Result<(), String> {
    encrypt_and_store_secret(
        store,
        state,
        SETTINGS_KEY_PASSPHRASE_PREFIX,
        session_id,
        passphrase,
    )
}

fn encrypt_and_store_secret(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    prefix: &str,
    session_id: i64,
    secret: &str,
) -> Result<(), String> {
    let key = state.get_key()?;
    let encrypted = encrypt_with_key(secret.as_bytes(), &key)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&encrypted);
    store.set_setting(&format!("{}{}", prefix, session_id), &encoded)?;
    Ok(())
}

pub fn get_encrypted_password(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    session_id: i64,
) -> Result<String, String> {
    get_encrypted_password_internal(store, state, session_id)
}

fn get_encrypted_password_internal(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    session_id: i64,
) -> Result<String, String> {
    get_encrypted_secret_internal(store, state, SETTINGS_PWD_PREFIX, session_id)
}

pub fn get_encrypted_key_passphrase(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    session_id: i64,
) -> Result<String, String> {
    get_encrypted_secret_internal(store, state, SETTINGS_KEY_PASSPHRASE_PREFIX, session_id)
}

fn get_encrypted_secret_internal(
    store: &crate::session_store::SessionStore,
    state: &MasterPasswordState,
    prefix: &str,
    session_id: i64,
) -> Result<String, String> {
    let key = state.get_key()?;
    let setting_key = format!("{}{}", prefix, session_id);
    let stored = store
        .get_setting(&setting_key)?
        .ok_or_else(|| format!("No stored secret for session {}", session_id))?;

    let blob = base64::engine::general_purpose::STANDARD
        .decode(&stored)
        .map_err(|e| format!("Invalid stored data: {}", e))?;

    let plaintext = decrypt_with_key(&blob, &key)?;
    String::from_utf8(plaintext).map_err(|_| "Invalid UTF-8 in decrypted password".to_string())
}

pub fn delete_encrypted_password(
    store: &crate::session_store::SessionStore,
    session_id: i64,
) -> Result<(), String> {
    store.delete_setting(&format!("{}{}", SETTINGS_PWD_PREFIX, session_id))
}

pub fn delete_encrypted_key_passphrase(
    store: &crate::session_store::SessionStore,
    session_id: i64,
) -> Result<(), String> {
    store.delete_setting(&format!("{}{}", SETTINGS_KEY_PASSPHRASE_PREFIX, session_id))
}
