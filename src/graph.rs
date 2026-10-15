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
    /// Set when items come from a local cache rather than the live service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
    /// The signed-in user's id when the backend knows it, so text output can say "You".
    #[serde(skip)]
    pub viewer: Option<String>,
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
    base_url: String,
}

impl GraphClient {
    pub fn new(token: String) -> Self {
        Self {
            // A write must never be replayed by transport policy or a redirect.
            http: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Microsoft Graph HTTP client configuration is valid"),
            token,
            base_url: GRAPH.into(),
        }
    }

    pub async fn me(&self) -> Result<Value, AppError> {
        self.get_value(&format!(
            "{}/me?$select=id,displayName,userPrincipalName,mail",
            self.base_url
        ))
        .await
    }

    /// Search directory-backed identities, never turn an unverified email into a recipient.
    pub async fn search_people(&self, query: &str, limit: usize) -> Result<Page, AppError> {
        validate_people_query(query, limit)?;
        let mut url = url::Url::parse(&format!("{}/users", self.base_url))
            .map_err(|e| AppError::Unexpected(e.to_string()))?;
        let literal = query.trim().replace('\'', "''");
        let filter = format!(
            "startswith(displayName,'{literal}') or startswith(mail,'{literal}') or startswith(userPrincipalName,'{literal}')"
        );
        url.query_pairs_mut()
            .append_pair("$select", "id,displayName,mail,userPrincipalName")
            .append_pair("$filter", &filter)
            .append_pair("$top", &limit.to_string());
        let value = self.get_value(url.as_str()).await.map_err(|error| match error {
            AppError::Permission(_) => error.with_hint("Recipient search needs User.ReadBasic.All. Sign in again with `teams auth login` and grant directory-profile access; your organization may restrict consent or guest directory access."),
            _ => error,
        })?;
        let page: GraphPage = serde_json::from_value(value)
            .map_err(|e| AppError::Api(format!("invalid Graph recipient response: {e}")))?;
        let truncated = page.next_link.is_some() || page.value.len() > limit;
        let items = page
            .value
            .iter()
            .take(limit)
            .map(normalize_person)
            .collect::<Result<_, _>>()?;
        Ok(Page {
            items,
            next_cursor: None,
            next_offset: None,
            truncated,
            source: None,
            viewer: None,
        })
    }

    /// Graph reuses the existing one-on-one chat for this pair of users.
    /// This creates no message, and a possibly accepted request is never retried.
    pub async fn create_chat(&self, recipient: &str) -> Result<Value, AppError> {
        if !is_directory_id(recipient) {
            return Err(AppError::InvalidInput(
                "--recipient must be a directory user ID returned by `teams people search`".into(),
            ));
        }
        let person = self
            .get_value(&format!(
                "{}/users/{recipient}?$select=id,displayName,mail,userPrincipalName",
                self.base_url
            ))
            .await?;
        if person["id"]
            .as_str()
            .is_none_or(|id| !id.eq_ignore_ascii_case(recipient))
        {
            return Err(AppError::Api(
                "Graph did not confirm the selected recipient's identity".into(),
            ));
        }
        let me = self.me().await?;
        let viewer = me["id"]
            .as_str()
            .filter(|id| is_directory_id(id))
            .ok_or_else(|| {
                AppError::Api("Graph did not return the signed-in user's directory ID".into())
            })?;
        if viewer.eq_ignore_ascii_case(recipient) {
            return Err(AppError::InvalidInput("Choose another person; Graph one-on-one chat creation does not support a chat with yourself".into()));
        }
        let role = self.recipient_role(recipient).await?;
        let body = one_on_one_body(viewer, recipient, role);
        let response = self
            .http
            .post(format!("{}/chats", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                if error.is_connect() || error.is_builder() {
                    AppError::Unexpected(format!("Microsoft Graph is unreachable: {error}"))
                } else {
                    unconfirmed_chat()
                }
            })?;
        if response.status().is_server_error() || response.status().as_u16() == 408 {
            return Err(unconfirmed_chat());
        }
        let value: Value = self
            .checked(response)
            .await?
            .json()
            .await
            .map_err(|_| unconfirmed_chat())?;
        if value["id"].as_str().is_none_or(str::is_empty) || value["chatType"] != "oneOnOne" {
            return Err(unconfirmed_chat());
        }
        Ok(value)
    }

    async fn recipient_role(&self, recipient: &str) -> Result<&'static str, AppError> {
        // Filtering on userType is supported by /users. Request only the basic ID,
        // rather than a broader profile scope or inferring roles from a UPN.
        for (user_type, role) in [("Guest", "guest"), ("Member", "owner")] {
            let mut url = url::Url::parse(&format!("{}/users", self.base_url))
                .map_err(|error| AppError::Unexpected(error.to_string()))?;
            url.query_pairs_mut()
                .append_pair("$select", "id")
                .append_pair(
                    "$filter",
                    &format!("id eq '{recipient}' and userType eq '{user_type}'"),
                )
                .append_pair("$top", "1");
            let value = self.get_value(url.as_str()).await?;
            let page: GraphPage = serde_json::from_value(value).map_err(|_| {
                AppError::Api("Graph did not confirm the selected recipient's chat role".into())
            })?;
            if page.next_link.is_some() || page.value.len() > 1 {
                return Err(AppError::Api(
                    "Graph returned an ambiguous recipient role".into(),
                ));
            }
            if let Some(person) = page.value.first() {
                if person["id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(recipient))
                {
                    return Ok(role);
                }
                return Err(AppError::Api(
                    "Graph did not confirm the selected recipient's chat role".into(),
                ));
            }
        }
        Err(AppError::Unsupported("Graph could not confirm whether this directory recipient is a member or guest; open the conversation in Teams instead. No conversation was created and no message was sent".into()))
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
            .map_err(|e| {
                if e.is_connect() || e.is_builder() {
                    AppError::Unexpected(format!("Microsoft Graph is unreachable: {e}"))
                } else {
                    unconfirmed()
                }
            })?;
        // Once the request has left, a server fault or an unreadable answer can hide a
        // delivered message; never let a caller treat that as safe to resend.
        if matches!(response.status().as_u16(), 500 | 502 | 504) {
            return Err(unconfirmed());
        }
        self.checked(response)
            .await?
            .json()
            .await
            .map_err(|_| unconfirmed())
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
            source: None,
            viewer: None,
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
            source: None,
            viewer: None,
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

fn unconfirmed() -> AppError {
    AppError::Unconfirmed(
        "Microsoft Graph did not confirm the message; it may still be delivered. Check the conversation before sending again".into(),
    )
}

fn unconfirmed_chat() -> AppError {
    AppError::Unconfirmed("Microsoft Graph did not confirm the conversation; it may have been created. Check your chats before trying again. No message was sent".into())
}

pub fn validate_people_query(query: &str, limit: usize) -> Result<(), AppError> {
    if !(2..=256).contains(&query.trim().chars().count()) || query.chars().any(char::is_control) {
        return Err(AppError::InvalidInput(
            "recipient search must contain 2–256 characters and no control characters".into(),
        ));
    }
    if !(1..=50).contains(&limit) {
        return Err(AppError::InvalidInput(
            "recipient search limit must be between 1 and 50".into(),
        ));
    }
    Ok(())
}

fn is_directory_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn normalize_person(person: &Value) -> Result<Value, AppError> {
    let id = person["id"]
        .as_str()
        .filter(|id| is_directory_id(id))
        .ok_or_else(|| {
            AppError::Api("Graph returned a recipient without a valid directory ID".into())
        })?;
    let email = person["mail"]
        .as_str()
        .filter(|mail| !mail.trim().is_empty())
        .or_else(|| {
            person["userPrincipalName"]
                .as_str()
                .filter(|mail| !mail.trim().is_empty())
        });
    let name = person["displayName"]
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .or(email)
        .unwrap_or(id);
    let mut value = serde_json::json!({"id":id,"displayName":name,"source":"graph"});
    if let Some(email) = email {
        value["email"] = Value::String(email.into());
    }
    Ok(value)
}

fn one_on_one_body(viewer: &str, recipient: &str, recipient_role: &str) -> Value {
    serde_json::json!({"chatType":"oneOnOne","members":[
        {"@odata.type":"#microsoft.graph.aadUserConversationMember","roles":["owner"],"user@odata.bind":format!("{GRAPH}/users('{viewer}')")},
        {"@odata.type":"#microsoft.graph.aadUserConversationMember","roles":[recipient_role],"user@odata.bind":format!("{GRAPH}/users('{recipient}')")}
    ]})
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
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, method, path, query_param},
    };

    const VIEWER: &str = "11111111-1111-4111-8111-111111111111";
    const RECIPIENT: &str = "22222222-2222-4222-8222-222222222222";

    fn client(server: &MockServer) -> super::GraphClient {
        let mut client = super::GraphClient::new("test-token".into());
        client.base_url = server.uri();
        client
    }

    async fn directory(server: &MockServer, recipient_upn: &str, user_type: &str) {
        Mock::given(method("GET")).and(path(format!("/users/{RECIPIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": RECIPIENT, "displayName":"Alex", "mail":"alex@example.test", "userPrincipalName":recipient_upn})))
            .expect(1).mount(server).await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":VIEWER})))
            .expect(1)
            .mount(server)
            .await;
        for kind in ["Guest", "Member"] {
            Mock::given(method("GET"))
                .and(path("/users"))
                .and(query_param("$select", "id"))
                .and(query_param("$filter", format!("id eq '{RECIPIENT}' and userType eq '{kind}'")))
                .and(query_param("$top", "1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":if user_type == kind { vec![json!({"id":RECIPIENT})] } else { vec![] }})))
                .expect(if user_type == "Guest" && kind == "Member" { 0 } else { 1 })
                .mount(server).await;
        }
    }

    #[tokio::test]
    async fn recipient_search_escapes_odata_literals_and_keeps_only_verified_identities() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/users"))
            .and(query_param("$filter", "startswith(displayName,'O''Brien & Co') or startswith(mail,'O''Brien & Co') or startswith(userPrincipalName,'O''Brien & Co')"))
            .and(query_param("$select", "id,displayName,mail,userPrincipalName"))
            .and(query_param("$top", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"id":RECIPIENT,"displayName":"O'Brien & Co","mail":"alex@example.test","ignored":"secret"}],"@odata.nextLink":"https://graph.microsoft.com/v1.0/users?$skiptoken=opaque"})))
            .expect(1).mount(&server).await;
        let page = client(&server)
            .search_people(" O'Brien & Co ", 1)
            .await
            .unwrap();
        assert_eq!(
            page.items,
            vec![
                json!({"id":RECIPIENT,"displayName":"O'Brien & Co","email":"alex@example.test","source":"graph"})
            ]
        );
        assert!(page.truncated);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn malformed_recipient_search_results_fail_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"value":[{"displayName":"Alex","mail":"alex@example.test"}]}),
            ))
            .mount(&server)
            .await;
        assert!(matches!(
            client(&server).search_people("Alex", 20).await,
            Err(crate::error::AppError::Api(_))
        ));
    }

    #[tokio::test]
    async fn creating_a_chat_uses_confirmed_directory_ids_and_sends_no_message() {
        let server = MockServer::start().await;
        // A converted guest can keep an #EXT# UPN while being a Member.
        directory(&server, "alex_example.test#EXT#@tenant.example", "Member").await;
        Mock::given(method("POST"))
            .and(path("/chats"))
            .and(body_json(super::one_on_one_body(
                VIEWER, RECIPIENT, "owner",
            )))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({"id":"existing-chat","chatType":"oneOnOne"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let value = client(&server).create_chat(RECIPIENT).await.unwrap();
        assert_eq!(value["id"], "existing-chat");
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn custom_directory_guests_receive_the_required_guest_role() {
        let server = MockServer::start().await;
        directory(&server, "custom-upn@tenant.example", "Guest").await;
        Mock::given(method("POST"))
            .and(path("/chats"))
            .and(body_json(super::one_on_one_body(
                VIEWER, RECIPIENT, "guest",
            )))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({"id":"guest-chat","chatType":"oneOnOne"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert!(client(&server).create_chat(RECIPIENT).await.is_ok());
    }

    #[tokio::test]
    async fn uncertain_chat_creation_is_not_retried() {
        for response in [
            ResponseTemplate::new(503),
            ResponseTemplate::new(408),
            ResponseTemplate::new(201).set_body_string("invalid json"),
        ] {
            let server = MockServer::start().await;
            directory(&server, "alex@example.test", "Member").await;
            Mock::given(method("POST"))
                .and(path("/chats"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let error = client(&server).create_chat(RECIPIENT).await.unwrap_err();
            assert_eq!(error.contract().kind, "unconfirmed");
            assert!(!error.contract().retryable);
            assert!(error.to_string().contains("No message was sent"));
            assert_eq!(server.received_requests().await.unwrap().len(), 5);
        }
    }

    #[tokio::test]
    async fn an_unconfirmed_directory_role_stops_before_creating_a_chat() {
        let server = MockServer::start().await;
        directory(&server, "alex@example.test", "Unknown").await;
        let error = client(&server).create_chat(RECIPIENT).await.unwrap_err();
        assert_eq!(error.contract().kind, "unsupported");
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn creation_redirects_cannot_replay_the_post() {
        let server = MockServer::start().await;
        directory(&server, "alex@example.test", "Member").await;
        Mock::given(method("POST"))
            .and(path("/chats"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/replayed", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert!(client(&server).create_chat(RECIPIENT).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn invalid_inputs_fail_before_any_request() {
        let server = MockServer::start().await;
        let client = client(&server);
        for query in ["", "a", "Alex\nInjected", &"a".repeat(257)] {
            assert_eq!(
                client
                    .search_people(query, 20)
                    .await
                    .unwrap_err()
                    .contract()
                    .kind,
                "invalid_input"
            );
        }
        for id in [
            "alex@example.test",
            "../me",
            "22222222-2222-4222-8222-22222222222z",
        ] {
            assert_eq!(
                client.create_chat(id).await.unwrap_err().contract().kind,
                "invalid_input"
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

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
