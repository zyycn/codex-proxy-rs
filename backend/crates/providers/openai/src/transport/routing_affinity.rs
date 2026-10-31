//! Responses 连接握手的路由 Cookie 快照，仅用于同一上游端点的 Guardian 请求

use std::{fmt, sync::Arc};

use reqwest::{
    cookie::{CookieStore, Jar},
    header::{COOKIE, HeaderMap, HeaderValue, SET_COOKIE},
};
use url::Url;
use uuid::Uuid;

const ROUTING_COOKIE_NAMES: [&str; 2] = ["__oailb", "__cflb"];

#[derive(Clone)]
pub(crate) struct ChatGptRoutingAffinity {
    id: Uuid,
    url: Url,
    jar: Arc<Jar>,
}

impl fmt::Debug for ChatGptRoutingAffinity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptRoutingAffinity")
            .finish_non_exhaustive()
    }
}

impl ChatGptRoutingAffinity {
    pub(crate) fn from_response(endpoint: &str, headers: &HeaderMap) -> Option<Self> {
        let url = cookie_url(endpoint)?;
        let mut cookies = Vec::with_capacity(2);
        for name in ROUTING_COOKIE_NAMES {
            let mut matching = headers.get_all(SET_COOKIE).iter().filter(|header| {
                header
                    .to_str()
                    .ok()
                    .and_then(|value| value.split_once('='))
                    .is_some_and(|(candidate, _)| candidate.trim() == name)
            });
            if let Some(cookie) = matching.next() {
                // 与 Codex 0.166 的连接快照一致，歧义或超长的 pin 不用于亲和
                if cookie.as_bytes().len() > 8192 || matching.next().is_some() {
                    return None;
                }
                cookies.push(cookie);
            }
        }
        let jar = Jar::default();
        jar.set_cookies(&mut cookies.into_iter(), &url);
        jar.cookies(&url)?;
        Some(Self {
            id: Uuid::new_v4(),
            url,
            jar: Arc::new(jar),
        })
    }

    pub(crate) const fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn apply(&self, endpoint: &str, headers: &mut HeaderMap) {
        let Some(url) = cookie_url(endpoint) else {
            return;
        };
        // 网关可配置 OAuth backend；快照只信任实际握手的 origin 与路径
        if url.origin() != self.url.origin() || url.path() != self.url.path() {
            return;
        }
        let Some(pin) = self.jar.cookies(&url) else {
            return;
        };
        let Ok(pin) = pin.to_str() else { return };
        let mut parts: Vec<_> = headers
            .get_all(COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .map(str::trim)
            .filter(|part| {
                !part.is_empty()
                    && !part
                        .split_once('=')
                        .is_some_and(|(name, _)| ROUTING_COOKIE_NAMES.contains(&name.trim()))
            })
            .collect();
        parts.push(pin);
        if let Ok(mut value) = HeaderValue::from_str(&parts.join("; ")) {
            value.set_sensitive(true);
            headers.insert(COOKIE, value);
        }
    }
}

fn cookie_url(endpoint: &str) -> Option<Url> {
    let mut url = Url::parse(endpoint).ok()?;
    let scheme = match url.scheme() {
        "https" | "wss" => "https",
        "http" | "ws" => "http",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    Some(url)
}
