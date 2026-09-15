//! 控制面会话 Cookie 的同源 HTTP / HTTPS 属性与解析。

use axum::http::{HeaderMap, header::ORIGIN};
use url::Url;

pub(crate) const NAME: &str = "cpr_session";

pub(crate) fn attributes(headers: &HeaderMap) -> &'static str {
    // HTTPS 反代回源 HTTP 不改变浏览器 Origin。只有单个、严格合法的 HTTP Origin
    // 才允许省略 Secure；缺失、opaque 或非法来源继续 fail closed。
    let http_origin = headers.get_all(ORIGIN).iter().count() == 1
        && headers
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|origin| {
                Url::parse(origin).is_ok_and(|url| {
                    url.scheme() == "http" && url.origin().ascii_serialization() == origin
                })
            });
    if http_origin {
        "Path=/; HttpOnly; SameSite=Lax"
    } else {
        "Path=/; Secure; HttpOnly; SameSite=Lax"
    }
}

pub(crate) fn value(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (candidate, value) = part.trim().split_once('=')?;
        (candidate == NAME).then(|| value.to_owned())
    })
}
