use codex_proxy_rs::codex::accounts::{cookies::jar::CookieJar, models::catalog::ModelCatalog};

#[test]
fn accounts_exports_account_scoped_assets() {
    let _cookie_jar_type = std::any::type_name::<CookieJar>();
    let _model_catalog_type = std::any::type_name::<ModelCatalog>();
}
