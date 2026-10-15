//! A verified page connection kept for the life of the process, so only the first read
//! pays for verification and the socket handshake. Sends never reuse it: each one verifies
//! afresh and evaluates exactly once.
use super::{ConnectionMetadata, ProbeStatus, avatars, cdp::Cdp, chat_id, valid_body, valid_id};
use crate::config::DesktopAccount;
use crate::error::AppError;
use serde_json::{Value, json};

const READ_SCRIPT: &str = include_str!("../desktop.js");
const SEND_SCRIPT: &str = include_str!("../desktop_send.js");
const PEOPLE_SCRIPT: &str = include_str!("../desktop_people.js");
const CACHE_SCRIPT: &str = include_str!("../desktop_cache.js");
// Teams can cache lone UTF-16 surrogates. JSON.stringify escapes them, but Rust's
// JSON strings require Unicode scalar values. Repair only the projected output.
const READ_JSON: &str = r#"(value => JSON.stringify(value, (_key, item) =>
    typeof item === 'string' ? item.toWellFormed() : item))"#;

/// Platform-specific verification before any session access. Raw target URLs remain
/// internal; only sanitized connection metadata can be returned to the caller.
pub trait Verifier: Send + Sync {
    fn core_only(&self, _port: u16) -> bool {
        false
    }
    fn verify(
        &self,
        port: u16,
    ) -> impl std::future::Future<Output = Result<(ConnectionMetadata, Vec<Value>), AppError>> + Send;
}

pub struct Session<S> {
    system: S,
    page: Option<(u16, Cdp, bool, DesktopAccount)>,
}

impl<S: Verifier> Session<S> {
    pub fn new(system: S) -> Self {
        Self { system, page: None }
    }

    /// Always verifies afresh; never touches the page.
    pub async fn probe(&self, port: u16) -> Result<ConnectionMetadata, AppError> {
        valid_port(port)?;
        Ok(self.system.verify(port).await?.0)
    }

    /// Answers a read request with `{"error": code}` or `{"result": <evaluate result>}`.
    pub async fn read(&mut self, request: &Value) -> Result<Value, AppError> {
        let (port, params) = read_params(request)?;
        let core = self.system.core_only(port);
        let account = DesktopAccount::from_request(&request["account_type"])?;
        if let Some((connected, page, personal, scope)) = &mut self.page
            && *connected == port
            && *scope == account
            && let Ok(mut result) = page
                .evaluate(&core_guard(&read_expression(&params, *personal), core))
                .await
            && !origin_changed(&result)
        {
            if params["query"] == "chats" {
                avatars::enrich(page, &mut result, *personal).await;
            }
            return Ok(json!({"result": result}));
        }
        // No page yet, another port, or a failed socket: verify again before any read.
        self.page = None;
        let Ok((metadata, targets)) = self.system.verify(port).await else {
            return Ok(refusal(ProbeStatus::MetadataProbeFailed));
        };
        if metadata.status != ProbeStatus::MetadataOk {
            return Ok(refusal_for(&metadata));
        }
        let candidates = match selected_pages(&targets, account) {
            Ok(pages) => pages,
            Err(refused) => return Ok(refused),
        };
        let Some(target) = candidates.into_iter().next() else {
            return Ok(json!({"error": "no_target"}));
        };
        let Ok(mut page) = Cdp::connect(port, target.id).await else {
            return Ok(json!({"error": "read_failed"}));
        };
        match page
            .evaluate(&core_guard(
                &read_expression(&params, target.personal),
                core,
            ))
            .await
        {
            Ok(mut result) => {
                if origin_changed(&result) {
                    return Ok(json!({"error": "no_target"}));
                }
                if params["query"] == "chats" {
                    avatars::enrich(&mut page, &mut result, target.personal).await;
                }
                self.page = Some((port, page, target.personal, account));
                Ok(json!({"result": result}))
            }
            Err(_) => Ok(json!({"error": "read_failed"})),
        }
    }

    /// Whether a send could start: `{"ok": true}` or `{"error": code}`. Sends nothing.
    pub async fn send_check(&self, port: u16, account: DesktopAccount) -> Result<Value, AppError> {
        valid_port(port)?;
        Ok(match self.main_page(port, account).await {
            Ok(_) => json!({"ok": true}),
            Err(refused) => refused,
        })
    }

    /// Sends one message: verifies the listener, finds the Teams window and evaluates the
    /// send script exactly once. A lost answer is `unconfirmed`, never retried.
    pub async fn send(&self, request: &Value) -> Result<Value, AppError> {
        let (port, params) = send_params(request)?;
        let account = DesktopAccount::from_request(&request["account_type"])?;
        let (mut page, personal) = match self.main_page(port, account).await {
            Ok(page) => page,
            Err(refused) => return Ok(refused),
        };
        Ok(
            match page
                .evaluate(&core_guard(
                    &scoped_expression(
                        &format!("({SEND_SCRIPT})({params},({CACHE_SCRIPT})).then(JSON.stringify)"),
                        personal,
                    ),
                    self.system.core_only(port),
                ))
                .await
            {
                Ok(result) => json!({"result": result}),
                Err(_) => json!({"error": "unconfirmed"}),
            },
        )
    }

    /// Search is read-only. Chat creation is evaluated once, never retried.
    pub async fn people(&self, request: &Value) -> Result<Value, AppError> {
        let port = request["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .ok_or_else(|| AppError::InvalidInput("invalid desktop port".into()))?;
        valid_port(port)?;
        let account = DesktopAccount::from_request(&request["account_type"])?;
        let create = request["op"] == "create";
        if create {
            super::check_recipient(request["recipient"].as_str().unwrap_or_default())?;
        } else if request["op"] == "search" {
            crate::graph::validate_people_query(
                request["query"].as_str().unwrap_or_default(),
                request["limit"].as_u64().unwrap_or_default() as usize,
            )?;
        } else {
            return Err(AppError::InvalidInput("invalid recipient operation".into()));
        }
        let (mut page, personal) = match self.main_page(port, account).await {
            Ok(page) => page,
            Err(refused) => return Ok(refused),
        };
        let mut params = request.clone();
        params["personal"] = json!(personal);
        let expression = scoped_expression(
            &format!("({PEOPLE_SCRIPT})({params},({CACHE_SCRIPT})).then({READ_JSON})"),
            personal,
        );
        Ok(
            match page
                .evaluate(&core_guard(&expression, self.system.core_only(port)))
                .await
            {
                Ok(result) => json!({"result":result}),
                Err(_) => json!({"error":if create { "unconfirmed" } else { "search_failed" }}),
            },
        )
    }

    /// A fresh connection to the Teams window that hosts the send client, or a refusal.
    async fn main_page(&self, port: u16, account: DesktopAccount) -> Result<(Cdp, bool), Value> {
        let Ok((metadata, targets)) = self.system.verify(port).await else {
            return Err(refusal(ProbeStatus::MetadataProbeFailed));
        };
        if metadata.status != ProbeStatus::MetadataOk {
            return Err(refusal_for(&metadata));
        }
        let candidates = selected_pages(&targets, account)?;
        if candidates.is_empty() {
            return Err(json!({"error": "no_target"}));
        }
        let probe = format!("({SEND_SCRIPT})({{\"probe\":true}}).then(JSON.stringify)");
        for target in candidates {
            let Ok(mut page) = Cdp::connect(port, target.id).await else {
                continue;
            };
            let main = page
                .evaluate(&core_guard(
                    &scoped_expression(&probe, target.personal),
                    self.system.core_only(port),
                ))
                .await
                .is_ok_and(|result| result["result"]["value"].as_str() == Some(r#"{"main":true}"#));
            if main {
                return Ok((page, target.personal));
            }
        }
        Err(json!({"error": "layout"}))
    }
}

fn core_guard(expression: &str, core: bool) -> String {
    if core {
        format!(
            "(window.__teamsCliCore?.ready===true?({expression}):JSON.stringify({{error:'core_not_ready'}}))"
        )
    } else {
        expression.to_owned()
    }
}

fn read_expression(params: &Value, personal: bool) -> String {
    let mut params = params.clone();
    params["personal"] = json!(personal);
    scoped_expression(
        &format!("({READ_SCRIPT})({params},({CACHE_SCRIPT})).then({READ_JSON})"),
        personal,
    )
}

/// A page can navigate after discovery or between reused reads. Check its current
/// origin in the same evaluation before touching cache data or the send client.
fn scoped_expression(expression: &str, personal: bool) -> String {
    let origins = if personal {
        json!(["https://teams.live.com"])
    } else {
        json!([
            "https://teams.microsoft.com",
            "https://teams.cloud.microsoft"
        ])
    };
    format!(
        "({origins}.includes(globalThis.location?.origin)?({expression}):JSON.stringify({{error:'no_target'}}))"
    )
}

fn origin_changed(result: &Value) -> bool {
    result["result"]["value"].as_str() == Some(r#"{"error":"no_target"}"#)
}

/// Revalidates a send request and builds the page parameters; they reach the page only as
/// a JSON literal.
fn send_params(request: &Value) -> Result<(u16, Value), AppError> {
    let invalid = || AppError::InvalidInput("invalid desktop send request".into());
    let port = request["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(invalid)?;
    valid_port(port)?;
    let chat = request["chat_id"]
        .as_str()
        .filter(|chat| chat_id(chat))
        .ok_or_else(invalid)?;
    let content = request["body"]
        .as_str()
        .filter(|body| valid_body(body))
        .ok_or_else(invalid)?;
    let id = request["client_message_id"]
        .as_str()
        .filter(|id| id.len() == 18 && id.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(invalid)?;
    Ok((
        port,
        json!({"chat": chat, "content": content, "clientMessageId": id}),
    ))
}

fn refusal(status: ProbeStatus) -> Value {
    json!({"error": status})
}

/// Carries the sanitized diagnosis so the caller can explain the refusal.
fn refusal_for(metadata: &ConnectionMetadata) -> Value {
    json!({"error": metadata.status, "connection": metadata})
}

fn valid_port(port: u16) -> Result<(), AppError> {
    if port < 1024 {
        return Err(AppError::InvalidInput(
            "desktop port must be between 1024 and 65535".into(),
        ));
    }
    Ok(())
}

/// Revalidates a read request and builds the page parameters; they reach the page
/// only as a JSON literal.
fn read_params(request: &Value) -> Result<(u16, Value), AppError> {
    let invalid = || AppError::InvalidInput("invalid desktop read request".into());
    let port = request["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(invalid)?;
    valid_port(port)?;
    let query = request["query"].as_str().ok_or_else(invalid)?;
    let limit = request["limit"]
        .as_u64()
        .filter(|l| (1..=200).contains(l))
        .ok_or_else(invalid)?;
    let chat = match (query, request["chat_id"].as_str()) {
        ("chats", None) => "",
        ("messages", Some(chat)) if chat_id(chat) => chat,
        _ => return Err(invalid()),
    };
    let after = match &request["after"] {
        Value::Null => Value::Null,
        after => {
            let t = after["t"]
                .as_i64()
                .filter(|t| *t >= 0)
                .ok_or_else(invalid)?;
            let id = after["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or_else(invalid)?;
            json!({"t": t, "id": id})
        }
    };
    Ok((
        port,
        json!({"query": query, "chat": chat, "limit": limit, "after": after}),
    ))
}

/// The Teams pages in endpoint order; their ids become part of the socket path, so they
/// are validated.
#[derive(Debug, PartialEq)]
struct TeamsPage<'a> {
    id: &'a str,
    personal: bool,
}

fn teams_pages(targets: &[Value]) -> Vec<TeamsPage<'_>> {
    targets
        .iter()
        .filter_map(|target| {
            let url = url::Url::parse(target["url"].as_str()?).ok()?;
            let id = target["id"].as_str()?;
            let host = url.host_str()?;
            (target["type"] == "page"
                && url.scheme() == "https"
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
                && matches!(
                    host,
                    "teams.microsoft.com" | "teams.cloud.microsoft" | "teams.live.com"
                )
                && (8..=64).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(TeamsPage {
                id,
                personal: host == "teams.live.com",
            })
        })
        .collect()
}

fn selected_pages(targets: &[Value], account: DesktopAccount) -> Result<Vec<TeamsPage<'_>>, Value> {
    let pages: Vec<_> = teams_pages(targets)
        .into_iter()
        .filter(|page| match account {
            DesktopAccount::Auto => true,
            DesktopAccount::Personal => page.personal,
            DesktopAccount::WorkSchool => !page.personal,
        })
        .collect();
    if pages.iter().any(|page| page.personal) && pages.iter().any(|page| !page.personal) {
        return Err(json!({"error": "account_ambiguous"}));
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    #[test]
    fn core_reads_are_guarded_even_when_a_cached_page_navigates_to_full_ui() {
        let expression = super::core_guard("cache_read()", true);
        let output = std::process::Command::new("node")
            .args(["-e", r#"
                const assert=require('node:assert/strict'), vm=require('node:vm');
                let reads=0;const window={},context={window,cache_read(){reads++;return 42}};
                assert.equal(vm.runInNewContext(process.argv[1],context),JSON.stringify({error:'core_not_ready'}));
                assert.equal(reads,0);
                window.__teamsCliCore={ready:true};assert.equal(vm.runInNewContext(process.argv[1],context),42);
                assert.equal(reads,1);
                delete window.__teamsCliCore;vm.runInNewContext(process.argv[1],context);assert.equal(reads,1);
            "#, &expression])
            .output().expect("Node.js is required for desktop script checks");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    use super::*;
    use crate::desktop::{
        MAX_BODY, cdp::tests::devtools_server, owner::tests::FakeSystem, read_request,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn read_json_repairs_unpaired_utf16_and_preserves_valid_text() {
        let output = std::process::Command::new("node")
            .args([
                "-e",
                &format!(
                    r#"const value={{items:[{{topic:'a\ud800b\udc00c',body:'🙂 é 中文',literal:'\\ud800',nested:['\ud83d\ude42','\ud800\ud800\udc00']}}]}};
                    console.log(JSON.stringify({{raw:JSON.stringify(value),clean:({READ_JSON})(value)}}));"#
                ),
            ])
            .output()
            .expect("Node.js 20 or newer is required for desktop script tests");
        assert!(output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(serde_json::from_str::<Value>(result["raw"].as_str().unwrap()).is_err());
        let clean: Value = serde_json::from_str(result["clean"].as_str().unwrap()).unwrap();
        assert_eq!(
            clean,
            json!({"items":[{"topic":"a�b�c","body":"🙂 é 中文","literal":"\\ud800","nested":["🙂","�𐀀"]}]})
        );
    }

    #[test]
    fn current_origin_guard_blocks_other_surfaces_and_lookalikes_before_evaluation() {
        for (personal, origin, allowed) in [
            (true, "https://teams.live.com", true),
            (true, "https://teams.microsoft.com", false),
            (true, "https://teams.live.com.evil.example", false),
            (false, "https://teams.microsoft.com", true),
            (false, "https://teams.cloud.microsoft", true),
            (false, "https://teams.live.com", false),
            (false, "http://teams.microsoft.com", false),
        ] {
            let expression =
                scoped_expression("(++globalThis.calls,JSON.stringify({ran:true}))", personal);
            let output = std::process::Command::new("node").args([
                "-e",
                "const vm=require('node:vm');const context={location:{origin:process.argv[1]},calls:0};const value=vm.runInNewContext(process.argv[2],context);console.log(JSON.stringify({value:JSON.parse(value),calls:context.calls}));",
                origin, &expression,
            ]).output().expect("Node.js is required for desktop script tests");
            assert!(output.status.success());
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["calls"], if allowed { 1 } else { 0 });
            assert_eq!(
                result["value"],
                if allowed {
                    json!({"ran":true})
                } else {
                    json!({"error":"no_target"})
                }
            );
        }
    }

    #[tokio::test]
    async fn navigated_cached_pages_are_dropped_and_verified_again_before_reading() {
        let evaluations = Arc::new(AtomicUsize::new(0));
        let counter = evaluations.clone();
        let port = devtools_server(
            json!([{"type":"page","url":"https://teams.live.com/v2/","id":"ABCDEF12"}]),
            move |request| {
                let value = if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                    r#"{"error":"no_target"}"#
                } else {
                    "{}"
                };
                Some(
                    json!({"id":request["id"],"result":{"result":{"type":"string","value":value}}})
                        .to_string(),
                )
            },
        )
        .await;
        let mut session = Session::new(FakeSystem::teams());
        let request = read_request(port, None, 1, None).unwrap();
        for _ in 0..2 {
            assert_eq!(
                session.read(&request).await.unwrap()["result"]["result"]["value"],
                "{}"
            );
        }
        assert_eq!(evaluations.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn read_params_are_revalidated() {
        let (port, params) = read_params(&read_request(9222, None, 5, None).unwrap()).unwrap();
        assert_eq!(port, 9222);
        assert_eq!(
            params,
            json!({"query": "chats", "chat": "", "limit": 5, "after": null})
        );
        let base = read_request(9222, Some("19:a@thread.v2"), 5, None).unwrap();
        assert_eq!(read_params(&base).unwrap().1["chat"], "19:a@thread.v2");
        for (key, value) in [
            ("port", json!(80)),
            ("port", json!(70000)),
            ("query", json!("teams")),
            ("limit", json!(0)),
            ("limit", json!(201)),
            ("chat_id", json!("19:a'); x")),
            ("chat_id", Value::Null),
            ("after", json!({"t": -1, "id": "x"})),
            ("after", json!({"t": 1, "id": "a b"})),
        ] {
            let mut request = base.clone();
            request[key] = value;
            assert!(read_params(&request).is_err(), "{key}");
        }
    }

    #[test]
    fn only_teams_pages_with_hex_ids_are_selected() {
        let targets = [
            json!({"type": "service_worker", "url": "https://teams.microsoft.com/", "id": "AAAAAAAA"}),
            json!({"type": "page", "url": "https://teams.microsoft.com.evil.example/", "id": "BBBBBBBB"}),
            json!({"type": "page", "url": "https://teams.microsoft.com/v2/", "id": "../../x"}),
            json!({"type": "page", "url": "https://teams.cloud.microsoft/v2/", "id": "CCCCCCCC"}),
        ];
        assert_eq!(
            teams_pages(&targets),
            [TeamsPage {
                id: "CCCCCCCC",
                personal: false
            }]
        );
        assert!(teams_pages(&targets[..3]).is_empty());
    }

    #[test]
    fn personal_pages_require_the_exact_secure_origin_and_safe_ids() {
        let targets = [
            json!({"type":"page","url":"https://teams.live.com/v2/","id":"AAAAAAAA"}),
            json!({"type":"page","url":"https://teams.live.com:443","id":"BBBBBBBB"}),
            json!({"type":"page","url":"https://teams.live.com.evil.example/v2/","id":"CCCCCCCC"}),
            json!({"type":"page","url":"http://teams.live.com/v2/","id":"DDDDDDDD"}),
            json!({"type":"page","url":"https://teams.live.com:444/v2/","id":"EEEEEEEE"}),
            json!({"type":"page","url":"https://teams.live.com@evil.example/v2/","id":"FFFFFFFF"}),
            json!({"type":"page","url":"https://evil.example@teams.live.com/v2/","id":"11111111"}),
            json!({"type":"page","url":"https://teams.live.com/v2/","id":"../../x"}),
            json!({"type":"service_worker","url":"https://teams.live.com/v2/","id":"22222222"}),
        ];
        assert_eq!(
            teams_pages(&targets),
            [
                TeamsPage {
                    id: "AAAAAAAA",
                    personal: true
                },
                TeamsPage {
                    id: "BBBBBBBB",
                    personal: true
                },
            ]
        );
    }

    #[tokio::test]
    async fn personal_reads_keep_the_identity_context_when_reusing_the_page() {
        let evaluations = Arc::new(AtomicUsize::new(0));
        let counter = evaluations.clone();
        let port = devtools_server(
            json!([{"type":"page","url":"https://teams.live.com/v2/","id":"ABCDEF12"}]),
            move |request| {
                counter.fetch_add(1, Ordering::SeqCst);
                assert!(
                    request["params"]["expression"]
                        .as_str()
                        .unwrap()
                        .contains(r#""personal":true"#)
                );
                Some(
                    json!({"id":request["id"],"result":{"result":{"type":"string","value":"{}"}}})
                        .to_string(),
                )
            },
        )
        .await;
        let mut session = Session::new(FakeSystem::teams());
        for _ in 0..2 {
            assert_eq!(
                session
                    .read(&read_request(port, None, 5, None).unwrap())
                    .await
                    .unwrap()["result"]["result"]["value"],
                "{}"
            );
        }
        assert_eq!(evaluations.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn mixed_personal_and_work_pages_are_refused_before_evaluation() {
        let evaluations = Arc::new(AtomicUsize::new(0));
        let counter = evaluations.clone();
        let port = devtools_server(
            json!([
                {"type":"page","url":"https://teams.live.com/v2/","id":"AAAAAAAA"},
                {"type":"page","url":"https://teams.microsoft.com/v2/","id":"BBBBBBBB"},
            ]),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                None
            },
        )
        .await;
        let mut session = Session::new(FakeSystem::teams());
        let refused = json!({"error":"account_ambiguous"});
        assert_eq!(
            session
                .read(&read_request(port, None, 5, None).unwrap())
                .await
                .unwrap(),
            refused
        );
        assert_eq!(
            session
                .send_check(port, DesktopAccount::Auto)
                .await
                .unwrap(),
            refused
        );
        assert_eq!(session.send(&send_request(port)).await.unwrap(), refused);
        assert_eq!(evaluations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn explicit_account_selection_filters_pages_and_revalidates_cached_read_scope() {
        let targets = json!([
            {"type":"page","url":"https://teams.microsoft.com/v2/","id":"AAAAAAAA"},
            {"type":"page","url":"https://teams.live.com/v2/","id":"BBBBBBBB"},
        ]);
        let selected =
            selected_pages(targets.as_array().unwrap(), DesktopAccount::Personal).unwrap();
        assert_eq!(
            selected,
            [TeamsPage {
                id: "BBBBBBBB",
                personal: true
            }]
        );
        assert_eq!(
            selected_pages(targets.as_array().unwrap(), DesktopAccount::WorkSchool).unwrap(),
            [TeamsPage {
                id: "AAAAAAAA",
                personal: false
            }]
        );
        let evaluations = Arc::new(AtomicUsize::new(0));
        let counter = evaluations.clone();
        let port = devtools_server(targets, move |request| {
            let expression = request["params"]["expression"].as_str().unwrap();
            if expression.contains("\"query\":\"chats\"") {
                let personal = counter.fetch_add(1, Ordering::SeqCst) == 0;
                assert!(expression.contains(if personal {
                    "\"personal\":true"
                } else {
                    "\"personal\":false"
                }));
            }
            let value = if expression.contains("\"probe\":true") {
                "{\"main\":true}"
            } else {
                "{}"
            };
            Some(
                json!({"id":request["id"],"result":{"result":{"type":"string","value":value}}})
                    .to_string(),
            )
        })
        .await;
        let mut session = Session::new(FakeSystem::teams());
        for account in [DesktopAccount::Personal, DesktopAccount::WorkSchool] {
            let mut request = read_request(port, None, 1, None).unwrap();
            request["account_type"] = json!(account);
            assert!(
                session
                    .read(&request)
                    .await
                    .unwrap()
                    .get("result")
                    .is_some()
            );
            assert_eq!(
                session.send_check(port, account).await.unwrap(),
                json!({"ok":true})
            );
            let mut send = send_request(port);
            send["account_type"] = json!(account);
            assert!(session.send(&send).await.unwrap().get("result").is_some());
        }
        assert_eq!(evaluations.load(Ordering::SeqCst), 2);
        let mut invalid = read_request(port, None, 1, None).unwrap();
        invalid["account_type"] = json!("all");
        assert!(session.read(&invalid).await.is_err());
        let mut invalid = send_request(port);
        invalid["account_type"] = json!(true);
        assert!(session.send(&invalid).await.is_err());
        assert_eq!(evaluations.load(Ordering::SeqCst), 2);
        // A work/school-only endpoint cannot fall back when personal was selected.
        let work = [json!({"type":"page","url":"https://teams.microsoft.com/v2/","id":"AAAAAAAA"})];
        assert!(
            selected_pages(&work, DesktopAccount::Personal)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reads_reuse_the_page_and_reverify_after_a_failure() {
        let evaluations = Arc::new(AtomicUsize::new(0));
        let counter = evaluations.clone();
        let port = devtools_server(
            json!([{"type": "page", "url": "https://teams.microsoft.com/v2/", "id": "ABCDEF12"}]),
            move |request| {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let expression = request["params"]["expression"].as_str().unwrap();
                assert!(expression.contains(r#""query":"chats""#));
                // The second evaluation drops the socket, as when Teams reloads.
                (n != 1).then(|| {
                    json!({"id": request["id"], "result": {"result": {"type": "string", "value": "{}"}}})
                        .to_string()
                })
            },
        )
        .await;
        let mut session = Session::new(FakeSystem::teams());
        let request = read_request(port, None, 5, None).unwrap();
        for _ in 0..3 {
            let reply = session.read(&request).await.unwrap();
            assert_eq!(reply["result"]["result"]["value"], "{}");
        }
        assert_eq!(evaluations.load(Ordering::SeqCst), 4);
        assert!(session.probe(port).await.unwrap().endpoint_available());
    }

    #[tokio::test]
    async fn refusals_are_reported_as_codes() {
        let port = devtools_server(json!([]), |_| None).await;
        let request = read_request(port, None, 5, None).unwrap();
        let mut session = Session::new(FakeSystem::teams());
        assert_eq!(
            session.read(&request).await.unwrap(),
            json!({"error": "no_target"})
        );
        let mut session = Session::new(FakeSystem {
            trusted: vec![],
            ..FakeSystem::teams()
        });
        let reply = session.read(&request).await.unwrap();
        assert_eq!(reply["error"], "non_teams_listener_refused");
        assert_eq!(reply["connection"]["teams_owned"], false);
    }

    fn send_request(port: u16) -> Value {
        crate::desktop::send_request(port, "19:a@thread.v2", "hi <b>\nthere").unwrap()
    }

    #[test]
    fn send_params_are_revalidated() {
        let base = send_request(9222);
        let (port, params) = send_params(&base).unwrap();
        assert_eq!(port, 9222);
        assert_eq!(params["chat"], "19:a@thread.v2");
        assert_eq!(params["content"], "hi <b>\nthere");
        assert_eq!(params["clientMessageId"].as_str().unwrap().len(), 18);
        for (key, value) in [
            ("port", json!(80)),
            ("chat_id", json!("19:a'); x")),
            ("chat_id", json!("48:other")),
            ("body", json!("  \n")),
            ("body", json!("a\0b")),
            ("body", json!("x".repeat(MAX_BODY + 1))),
            ("body", Value::Null),
            ("client_message_id", json!("12345")),
            ("client_message_id", json!("12345678901234567x")),
        ] {
            let mut request = base.clone();
            request[key] = value;
            assert!(send_params(&request).is_err(), "{key}");
        }
    }

    /// Three Teams pages; only the second hosts the send client. Counts send evaluations.
    async fn teams_window(sends: Arc<AtomicUsize>, answer_send: bool, origin: &str) -> u16 {
        devtools_server(
            json!([
                {"type": "page", "url": format!("{origin}/v2/"), "id": "AAAAAAAA"},
                {"type": "page", "url": format!("{origin}/v2/"), "id": "BBBBBBBB"},
                {"type": "page", "url": format!("{origin}/v2/"), "id": "CCCCCCCC"},
            ]),
            move |request| {
                let expression = request["params"]["expression"].as_str().unwrap();
                assert!(expression.contains("autoSendMessage"));
                let value = if expression.contains(r#"({"probe":true})"#) {
                    // The page id is not visible here; answer by probe order instead.
                    if sends.fetch_add(1000, Ordering::SeqCst) / 1000 == 1 {
                        r#"{"main":true}"#
                    } else {
                        r#"{"main":false}"#
                    }
                } else {
                    assert!(expression.contains(r#""chat":"19:a@thread.v2""#));
                    sends.fetch_add(1, Ordering::SeqCst);
                    if !answer_send {
                        return None;
                    }
                    r#"{"id":"1700000000000"}"#
                };
                Some(json!({"id": request["id"], "result": {"result": {"type": "string", "value": value}}}).to_string())
            },
        )
        .await
    }

    #[tokio::test]
    async fn sends_go_to_the_window_with_the_client_and_run_once() {
        for origin in ["https://teams.microsoft.com", "https://teams.live.com"] {
            let sends = Arc::new(AtomicUsize::new(0));
            let port = teams_window(sends.clone(), true, origin).await;
            let session = Session::new(FakeSystem::teams());
            assert_eq!(
                session
                    .send_check(port, DesktopAccount::Auto)
                    .await
                    .unwrap(),
                json!({"ok": true})
            );
            sends.store(0, Ordering::SeqCst);
            let reply = session.send(&send_request(port)).await.unwrap();
            assert_eq!(
                reply["result"]["result"]["value"],
                r#"{"id":"1700000000000"}"#
            );
            // Two probes (the first page is not the window), then one send.
            assert_eq!(sends.load(Ordering::SeqCst), 2001);
        }
    }

    #[tokio::test]
    async fn a_lost_send_answer_is_unconfirmed_and_not_retried() {
        let sends = Arc::new(AtomicUsize::new(0));
        let port = teams_window(sends.clone(), false, "https://teams.microsoft.com").await;
        let session = Session::new(FakeSystem::teams());
        assert_eq!(
            session.send(&send_request(port)).await.unwrap(),
            json!({"error": "unconfirmed"})
        );
        assert_eq!(sends.load(Ordering::SeqCst) % 1000, 1);
    }

    #[tokio::test]
    async fn sends_are_refused_without_a_verified_window() {
        let untrusted = Session::new(FakeSystem {
            trusted: vec![],
            ..FakeSystem::teams()
        });
        let sends = Arc::new(AtomicUsize::new(0));
        let port = teams_window(sends.clone(), true, "https://teams.microsoft.com").await;
        let reply = untrusted.send(&send_request(port)).await.unwrap();
        assert_eq!(reply["error"], "non_teams_listener_refused");
        assert_eq!(reply["connection"]["teams_owned"], false);
        assert_eq!(
            reply["connection"]["foreign_listeners"],
            json!(["ms-teams.exe"])
        );
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        let empty = devtools_server(json!([]), |_| None).await;
        let session = Session::new(FakeSystem::teams());
        assert_eq!(
            session
                .send_check(empty, DesktopAccount::Auto)
                .await
                .unwrap(),
            json!({"error": "no_target"})
        );
        let none = devtools_server(
            json!([{"type": "page", "url": "https://teams.microsoft.com/v2/", "id": "AAAAAAAA"}]),
            |request| {
                Some(json!({"id": request["id"], "result": {"result": {"type": "string", "value": r#"{"main":false}"#}}}).to_string())
            },
        )
        .await;
        assert_eq!(
            session.send(&send_request(none)).await.unwrap(),
            json!({"error": "layout"})
        );
    }
}
