//! 验证 Antigravity 授权、画像、目录、执行与传输合同

use provider_antigravity::ConfigError;
#[path = "../src/billing/mod.rs"]
#[allow(dead_code, unused_imports)]
mod billing;
#[path = "billing/mod.rs"]
mod billing_tests;
#[path = "credential/mod.rs"]
#[allow(dead_code, unused_imports)]
mod credential;
#[path = "../src/profile/mod.rs"]
#[allow(dead_code, unused_imports)]
mod profile;
#[path = "profile/mod.rs"]
mod profile_tests;
#[path = "../src/provider/mod.rs"]
#[allow(dead_code, unused_imports)]
mod provider;
#[path = "provider/mod.rs"]
mod provider_tests;
mod support;
#[path = "../src/transport/mod.rs"]
#[allow(dead_code, unused_imports)]
mod transport;
#[path = "transport/mod.rs"]
mod transport_tests;
