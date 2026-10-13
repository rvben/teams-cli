//! A verified page connection kept for the life of the process, so only the first read
//! pays for verification and the socket handshake.
use super::{
    ConnectionMetadata, ProbeStatus,
    cdp::Cdp,
    owner::{System, verify},
    valid_id,
};
use crate::error::AppError;
use serde_json::{Value, json};

const READ_SCRIPT: &str = include_str!("../desktop.js");

pub struct Session<S> {
    system: S,
    page: Option<(u16, Cdp)>,
}

impl<S: System> Session<S> {
    pub fn new(system: S) -> Self {
        Self { system, page: None }
    }

    /// Always verifies afresh; never touches the page.
    pub async fn probe(&self, port: u16) -> Result<ConnectionMetadata, AppError> {
        valid_port(port)?;
        Ok(verify(&self.system, port).await?.0)
    }

    /// Answers a read request with `{"error": code}` or `{"result": <evaluate result>}`.
    pub async fn read(&mut self, request: &Value) -> Result<Value, AppError> {
        let (port, params) = read_params(request)?;
        let expression = format!("({READ_SCRIPT})({params}).then(JSON.stringify)");
        if let Some((connected, page)) = &mut self.page
            && *connected == port
            && let Ok(result) = page.evaluate(&expression).await
        {
            return Ok(json!({"result": result}));
        }
        // No page yet, another port, or a failed socket: verify again before any read.
        self.page = None;
        let Ok((metadata, targets)) = verify(&self.system, port).await else {
            return Ok(refusal(ProbeStatus::MetadataProbeFailed));
        };
        if metadata.status != ProbeStatus::MetadataOk {
            return Ok(refusal(metadata.status));
        }
        let Some(target) = teams_page(&targets) else {
            return Ok(json!({"error": "no_target"}));
        };
        let Ok(mut page) = Cdp::connect(port, target).await else {
            return Ok(json!({"error": "read_failed"}));
        };
        match page.evaluate(&expression).await {
            Ok(result) => {
                self.page = Some((port, page));
                Ok(json!({"result": result}))
            }
            Err(_) => Ok(json!({"error": "read_failed"})),
        }
    }
}

fn refusal(status: ProbeStatus) -> Value {
    json!({"error": status})
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
        ("messages", Some(chat)) if chat.starts_with("19:") && valid_id(chat) => chat,
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

/// The first Teams page; its id becomes part of the socket path, so it is validated.
fn teams_page(targets: &[Value]) -> Option<&str> {
    targets.iter().find_map(|target| {
        let url = target["url"].as_str()?;
        let id = target["id"].as_str()?;
        (target["type"] == "page"
            && (url.starts_with("https://teams.microsoft.com/")
                || url.starts_with("https://teams.cloud.microsoft/"))
            && (8..=64).contains(&id.len())
            && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{cdp::tests::devtools_server, owner::tests::FakeSystem, read_request};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

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
        assert_eq!(teams_page(&targets), Some("CCCCCCCC"));
        assert_eq!(teams_page(&targets[..3]), None);
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
        assert_eq!(
            session.read(&request).await.unwrap(),
            json!({"error": "non_teams_listener_refused"})
        );
    }
}
