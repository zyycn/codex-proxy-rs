//! xAI Provider、凭据与上游传输的测试入口

mod admin;
#[path = "../src/billing/mod.rs"]
#[allow(dead_code, unused_imports)]
mod billing;
#[path = "billing/mod.rs"]
mod billing_tests;
mod credential;
mod oauth_http;
mod profile;
mod provider;
mod secret;
mod support;
mod transport;
