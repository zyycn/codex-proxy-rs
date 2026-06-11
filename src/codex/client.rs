use reqwest::Client;

pub fn build_reqwest_client(force_http11: bool) -> Result<Client, reqwest::Error> {
    // 中文注释：Codex Desktop 指纹依赖 reqwest/rustls 组合，升级前必须重新验证 TLS 行为。
    let builder = Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .deflate(true);
    let builder = if force_http11 {
        builder.http1_only()
    } else {
        builder
    };
    builder.build()
}
