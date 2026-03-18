use anyhow::{bail, Result};
use serde_json::Value;

pub struct ApiClient {
    pub base_url: String,
    pub user_id: String,
    pub api_key: String,
}

impl ApiClient {
    pub fn new(base_url: String, user_id: String, api_key: String) -> Self {
        Self {
            base_url,
            user_id,
            api_key,
        }
    }

    pub fn get_json(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let result = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("X-Bisque-User-Id", &self.user_id)
            .call();
        Self::parse_response(result)
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let body_str = serde_json::to_string(body)?;
        let result = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("X-Bisque-User-Id", &self.user_id)
            .set("Content-Type", "application/json")
            .send_string(&body_str);
        Self::parse_response(result)
    }

    fn parse_response(
        result: std::result::Result<ureq::Response, ureq::Error>,
    ) -> Result<Value> {
        match result {
            Ok(resp) => {
                let body = resp.into_string()?;
                if body.trim().is_empty() {
                    Ok(serde_json::json!({"ok": true}))
                } else {
                    Ok(serde_json::from_str(&body)?)
                }
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let detail = if body.is_empty() {
                    format!("HTTP {code}")
                } else {
                    truncate_safe(&body, 300)
                };
                bail!("Request failed ({code}): {detail}")
            }
            Err(e) => bail!("Request failed: {e}"),
        }
    }
}

fn truncate_safe(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = max_len;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
