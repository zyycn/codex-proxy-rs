use codex_proxy_rs::infra::crypto::SecretBox;
use secrecy::{ExposeSecret, SecretString};

#[test]
fn secret_box_encrypts_and_decrypts_without_storing_plaintext() {
    let secret_box = SecretBox::new([7u8; 32]);
    let plaintext = SecretString::new("rt_example_refresh_token".to_string().into());
    let ciphertext = secret_box.encrypt(&plaintext).unwrap();

    assert!(ciphertext.starts_with("v1:"));
    assert!(!ciphertext.contains("rt_example_refresh_token"));
    assert_eq!(
        secret_box.decrypt(&ciphertext).unwrap().expose_secret(),
        "rt_example_refresh_token"
    );
}

#[test]
fn secret_box_load_or_create_should_reuse_existing_master_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("master.key");
    let first_box = SecretBox::load_or_create(&key_path).unwrap();
    let ciphertext = first_box
        .encrypt(&SecretString::new("persisted-secret".to_string().into()))
        .unwrap();

    let second_box = SecretBox::load_or_create(&key_path).unwrap();
    let decrypted = second_box.decrypt(&ciphertext).unwrap();

    assert_eq!(decrypted.expose_secret(), "persisted-secret");
}
