use std::path::Path;

use provider_xai::XaiConfig;

#[test]
fn xai_config_accepts_only_the_fixed_official_protocol_shape() {
    let mut config: XaiConfig = serde_json::from_str("{}").expect("empty xAI config");
    config
        .resolve_and_validate(Path::new("/srv/gateway"))
        .expect("fixed official protocol");

    assert!(serde_json::from_str::<XaiConfig>(r#"{"client_version":"override"}"#).is_err());
}
