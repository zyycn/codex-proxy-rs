use codex_proxy_rs::crypto::SecretBox;
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
