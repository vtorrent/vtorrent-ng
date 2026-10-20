use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use vtorrent_migrate::crypter::{decrypt_private_key, DecryptedMasterKey};

fn decrypt_fixture(compressed: bool) -> bool {
    let secret = secp256k1::SecretKey::from_slice(&[0x42; 32]).unwrap();
    let public = secp256k1::PublicKey::from_secret_key(&secp256k1::Secp256k1::new(), &secret);
    let public = if compressed {
        public.serialize().to_vec()
    } else {
        public.serialize_uncompressed().to_vec()
    };
    let master = DecryptedMasterKey { key: [0x51; 32] };
    let iv = vtorrent_core::crypto::sha256d(&public);
    let mut buffer = [0u8; 48];
    buffer[..32].copy_from_slice(&secret.secret_bytes());
    let encrypted = cbc::Encryptor::<aes::Aes256>::new_from_slices(&master.key, &iv[..16])
        .unwrap()
        .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buffer, 32)
        .unwrap();
    decrypt_private_key(encrypted, &public, &master)
        .is_ok_and(|key| key.as_slice() == secret.secret_bytes())
}

#[test]
fn audit_compressed_legacy_key_control() {
    assert!(decrypt_fixture(true));
}

#[test]
fn audit_uncompressed_legacy_key_is_recoverable() {
    assert!(
        decrypt_fixture(false),
        "valid uncompressed ckey rejected despite correct master key and ciphertext"
    );
}
