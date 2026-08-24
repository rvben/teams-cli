use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

const GRAPH: &str = "https://graph.microsoft.com/v1.0";

#[derive(Debug, Serialize)]
pub struct Page {
    pub items: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
struct GraphPage {
    value: Vec<Value>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

pub struct GraphClient {
    http: reqwest::Client,
    token: String,
}

impl GraphClient {
    pub fn new(token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
        }
    }

    pub async fn me(&self) -> Result<Value, AppError> {
        self.get_value(&format!(
            "{GRAPH}/me?$select=id,displayName,userPrincipalName,mail"
        ))
        .await
    }

    pub async fn teams(&self, limit: u16, offset: usize) -> Result<Page, AppError> {
        self.get_offset_page(&format!("{GRAPH}/me/joinedTeams"), limit, offset)
            .await
    }

    pub async fn teams_available(&self) -> Result<(), AppError> {
        self.get_value(&format!("{GRAPH}/me/joinedTeams"))
            .await
            .map(|_| ())
    }

    pub async fn channels(&self, team: &str, limit: u16, offset: usize) -> Result<Page, AppError> {
        self.get_offset_page(
            &format!(
                "{GRAPH}/teams/{team}/channels?$select=id,displayName,description,membershipType"
            ),
            limit,
            offset,
        )
        .await
    }

    pub async fn chats(&self, limit: u16, cursor: Option<&str>) -> Result<Page, AppError> {
        self.get_page(
            cursor,
            &format!("{GRAPH}/me/chats?$expand=members&$top={limit}"),
            limit,
        )
        .await
    }

    pub async fn messages(
        &self,
        chat: Option<&str>,
        team: Option<&str>,
        channel: Option<&str>,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<Page, AppError> {
        let base = match (chat, team, channel) {
            (Some(chat), None, None) => format!("{GRAPH}/chats/{chat}/messages?$top={limit}"),
            (None, Some(team), Some(channel)) => {
                format!("{GRAPH}/teams/{team}/channels/{channel}/messages?$top={limit}")
            }
            _ => {
                return Err(AppError::InvalidInput(
                    "choose either --chat, or both --team and --channel".into(),
                ));
            }
        };
        self.get_page(cursor, &base, limit).await
    }

    pub async fn send(
        &self,
        chat: Option<&str>,
        team: Option<&str>,
        channel: Option<&str>,
        body: &str,
    ) -> Result<Value, AppError> {
        let url = match (chat, team, channel) {
            (Some(chat), None, None) => format!("{GRAPH}/chats/{chat}/messages"),
            (None, Some(team), Some(channel)) => {
                format!("{GRAPH}/teams/{team}/channels/{channel}/messages")
            }
            _ => {
                return Err(AppError::InvalidInput(
                    "choose either --chat, or both --team and --channel".into(),
                ));
            }
        };
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"body": {"contentType": "text", "content": body}}))
            .send()
            .await
            .map_err(|e| AppError::Unexpected(format!("Microsoft Graph is unreachable: {e}")))?;
        self.response_value(response).await
    }

    async fn get_value(&self, url: &str) -> Result<Value, AppError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| AppError::Unexpected(format!("Microsoft Graph is unreachable: {e}")))?;
        self.response_value(response).await
    }

    async fn get_page(
        &self,
        cursor: Option<&str>,
        first: &str,
        limit: u16,
    ) -> Result<Page, AppError> {
        let url = cursor.unwrap_or(first);
        if !is_graph_url(url) {
            return Err(AppError::InvalidInput(
                "--cursor must be an opaque Microsoft Graph v1.0 continuation URL".into(),
            ));
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| AppError::Unexpected(format!("Microsoft Graph is unreachable: {e}")))?;
        let response = self.checked(response).await?;
        let mut page: GraphPage = response
            .json()
            .await
            .map_err(|e| AppError::Api(format!("invalid Graph response: {e}")))?;
        let over_limit = page.value.len() > limit as usize;
        page.value.truncate(limit as usize);
        let truncated = over_limit || page.next_link.is_some();
        Ok(Page {
            items: page.value,
            next_cursor: page.next_link,
            next_offset: None,
            truncated,
        })
    }

    async fn get_offset_page(
        &self,
        first: &str,
        limit: u16,
        offset: usize,
    ) -> Result<Page, AppError> {
        let mut url = Some(first.to_string());
        let mut all = Vec::new();
        while let Some(current) = url {
            if !is_graph_url(&current) {
                return Err(AppError::Api(
                    "Graph returned an invalid continuation URL".into(),
                ));
            }
            let response = self
                .http
                .get(&current)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| {
                    AppError::Unexpected(format!("Microsoft Graph is unreachable: {e}"))
                })?;
            let page: GraphPage = self
                .checked(response)
                .await?
                .json()
                .await
                .map_err(|e| AppError::Api(format!("invalid Graph response: {e}")))?;
            all.extend(page.value);
            url = page.next_link;
            if all.len() > 10_000 {
                return Err(AppError::Api(
                    "Graph returned more than 10,000 records while preparing the requested page"
                        .into(),
                ));
            }
        }
        let total = all.len();
        let items = all.into_iter().skip(offset).take(limit as usize).collect();
        let consumed = offset.saturating_add(limit as usize);
        let truncated = consumed < total;
        Ok(Page {
            items,
            next_cursor: None,
            next_offset: truncated.then_some(consumed),
            truncated,
        })
    }

    async fn response_value(&self, response: reqwest::Response) -> Result<Value, AppError> {
        self.checked(response)
            .await?
            .json()
            .await
            .map_err(|e| AppError::Api(format!("invalid Graph response: {e}")))
    }

    async fn checked(&self, response: reqwest::Response) -> Result<reqwest::Response, AppError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        if status.as_u16() == 429 {
            return Err(AppError::RateLimit(retry_after));
        }
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Microsoft Graph rejected the request");
        match status.as_u16() {
            401 => Err(AppError::Auth(format!("{message}; run `teams auth login`"))),
            403 => Err(permission_error(message)),
            404 => Err(AppError::NotFound(message.into())),
            _ => Err(AppError::Api(format!("{message} ({status})"))),
        }
    }
}

fn permission_error(message: &str) -> AppError {
    let message = message.trim();
    let lower = message.to_ascii_lowercase();
    let hint = if lower.contains("hasn't been provisioned")
        || lower.contains("has not been provisioned")
        || lower.contains("valid office365 subscription")
        || lower.contains("valid microsoft 365 subscription")
    {
        "sign in with a work or school account that has Microsoft Teams enabled"
    } else {
        "check delegated scopes and tenant consent with `teams doctor`"
    };
    let separator = if matches!(message.chars().last(), Some('.' | '!' | '?')) {
        " "
    } else {
        "; "
    };
    AppError::Permission(format!("{message}{separator}{hint}"))
}

fn is_graph_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("graph.microsoft.com")
            && url.path().starts_with("/v1.0/")
    })
}

pub fn select_fields(page: &mut Page, fields: Option<&str>) -> Result<(), AppError> {
    let Some(fields) = fields else {
        return Ok(());
    };
    let names: Vec<&str> = fields
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return Err(AppError::InvalidInput(
            "--fields must name at least one field".into(),
        ));
    }
    for item in &mut page.items {
        let source = item
            .as_object()
            .ok_or_else(|| AppError::Api("Graph returned a non-object collection item".into()))?;
        let filtered = names
            .iter()
            .filter_map(|name| {
                source
                    .get(*name)
                    .cloned()
                    .map(|value| ((*name).to_string(), value))
            })
            .collect();
        *item = Value::Object(filtered);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn continuation_urls_cannot_exfiltrate_bearer_tokens() {
        assert!(super::is_graph_url(
            "https://graph.microsoft.com/v1.0/me/chats?$skiptoken=abc"
        ));
        assert!(!super::is_graph_url(
            "https://graph.microsoft.com.evil.example/v1.0/me/chats"
        ));
        assert!(!super::is_graph_url(
            "http://graph.microsoft.com/v1.0/me/chats"
        ));
    }

    #[test]
    fn unprovisioned_tenants_get_a_teams_specific_hint() {
        let error = super::permission_error(
            "Microsoft Teams hasn't been provisioned on the tenant. Ensure the tenant has a valid Office365 subscription.",
        );
        assert!(
            error
                .to_string()
                .contains("account that has Microsoft Teams enabled")
        );
        assert!(!error.to_string().contains(".;"));
    }
}
