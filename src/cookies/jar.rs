use std::collections::HashMap;

#[derive(Debug, Clone)]
struct StoredCookie {
    domain: String,
    name: String,
    value: String,
}

#[derive(Debug, Default, Clone)]
pub struct CookieJar {
    by_account: HashMap<String, Vec<StoredCookie>>,
}

impl CookieJar {
    pub fn capture_set_cookie(&mut self, account_id: &str, raw: &str) {
        let mut parts = raw.split(';').map(str::trim);
        let Some(name_value) = parts.next() else {
            return;
        };
        let Some((name, value)) = name_value.split_once('=') else {
            return;
        };
        let mut domain = "chatgpt.com".to_string();
        for part in parts {
            if let Some(value) = part.strip_prefix("Domain=") {
                domain = value.trim_start_matches('.').to_string();
            }
        }

        let account = self.by_account.entry(account_id.to_string()).or_default();
        account.retain(|cookie| !(cookie.domain == domain && cookie.name == name));
        account.push(StoredCookie {
            domain,
            name: name.to_string(),
            value: value.to_string(),
        });
    }

    pub fn cookie_header(&self, account_id: &str, domain: &str) -> Option<String> {
        // 中文注释：Cloudflare Cookie 必须按账号隔离，不能在不同 Codex 账号之间复用。
        let cookies = self.by_account.get(account_id)?;
        let pairs = cookies
            .iter()
            .filter(|cookie| domain_matches(domain, &cookie.domain))
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }
}

fn domain_matches(request_domain: &str, cookie_domain: &str) -> bool {
    request_domain == cookie_domain
        || request_domain
            .strip_suffix(cookie_domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}
