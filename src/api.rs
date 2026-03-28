use anyhow::{bail, Result};
use serde_json::Value;
use std::io::Read;

/// Response from a tool call — either JSON data or raw binary bytes.
pub enum ToolCallResponse {
    Json(Value),
    Binary {
        content_type: String,
        data: Vec<u8>,
    },
}

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

    /// POST a tool call and return either JSON or raw binary based on Content-Type.
    pub fn post_tool_call(&self, path: &str, body: &Value) -> Result<ToolCallResponse> {
        let url = format!("{}{}", self.base_url, path);
        let body_str = serde_json::to_string(body)?;
        let result = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("X-Bisque-User-Id", &self.user_id)
            .set("Content-Type", "application/json")
            .send_string(&body_str);

        match result {
            Ok(resp) => {
                let content_type = resp
                    .header("Content-Type")
                    .unwrap_or("application/json")
                    .to_string();

                if content_type.contains("application/json") || content_type.contains("text/") {
                    // JSON response — parse as before
                    let body = resp.into_string()?;
                    if body.trim().is_empty() {
                        Ok(ToolCallResponse::Json(serde_json::json!({"ok": true})))
                    } else {
                        Ok(ToolCallResponse::Json(serde_json::from_str(&body)?))
                    }
                } else {
                    // Binary response — read raw bytes
                    let mut data = Vec::new();
                    resp.into_reader().read_to_end(&mut data)?;
                    Ok(ToolCallResponse::Binary { content_type, data })
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
