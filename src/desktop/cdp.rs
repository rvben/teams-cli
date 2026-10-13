//! Minimal DevTools protocol client: one page socket, one `Runtime.evaluate` at a time.
use super::MAX_READ;
use crate::error::AppError;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const EVALUATE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Cdp {
    /// The socket URL is rebuilt from the verified port and a validated target id,
    /// never taken from the endpoint's advertised `webSocketDebuggerUrl`.
    pub async fn connect(port: u16, target: &str) -> Result<Self, AppError> {
        let failed = || AppError::Desktop("could not connect to the Teams page".into());
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_READ))
            .max_frame_size(Some(MAX_READ));
        let url = format!("ws://127.0.0.1:{port}/devtools/page/{target}");
        let (socket, _) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(url, Some(config), false),
        )
        .await
        .map_err(|_| failed())?
        .map_err(|_| failed())?;
        Ok(Self { socket, next_id: 0 })
    }

    /// Returns the `Runtime.evaluate` result object, including any `exceptionDetails`.
    pub async fn evaluate(&mut self, expression: &str) -> Result<Value, AppError> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({"id": id, "method": "Runtime.evaluate", "params": {
            "expression": expression, "awaitPromise": true, "returnByValue": true}});
        tokio::time::timeout(EVALUATE_TIMEOUT, async {
            let failed = || AppError::Desktop("the Teams page connection failed".into());
            self.socket
                .send(Message::text(request.to_string()))
                .await
                .map_err(|_| failed())?;
            while let Some(message) = self.socket.next().await {
                let Message::Text(text) = message.map_err(|_| failed())? else {
                    continue;
                };
                // Events and replies to other ids are skipped.
                let Ok(reply) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if reply["id"] != id {
                    continue;
                }
                return match reply.get("result") {
                    Some(result) if result.is_object() => Ok(result.clone()),
                    _ => Err(AppError::Desktop(
                        "the Teams page refused the read request".into(),
                    )),
                };
            }
            Err(failed())
        })
        .await
        .map_err(|_| AppError::Desktop("the Teams page did not answer in time".into()))?
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_tungstenite::accept_async;

    /// A loopback DevTools endpoint: `/json/version` and `/json/list` (with `targets`)
    /// over plain HTTP, and page sockets that answer each evaluate through `answer`
    /// after an unrelated event. `None` closes the socket.
    pub async fn devtools_server(
        targets: Value,
        answer: impl Fn(&Value) -> Option<String> + Send + Sync + 'static,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let answer = std::sync::Arc::new(answer);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (targets, answer) = (targets.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut head = [0u8; 32];
                    let n = stream.peek(&mut head).await.unwrap_or(0);
                    let line = String::from_utf8_lossy(&head[..n]).into_owned();
                    let body = if line.starts_with("GET /json/version ") {
                        Some(json!({"Browser": "Edg/fixture", "Protocol-Version": "1.3"}))
                    } else if line.starts_with("GET /json/list ") {
                        Some(targets)
                    } else {
                        None
                    };
                    if let Some(body) = body {
                        let mut request = [0u8; 4096];
                        let _ = stream.read(&mut request).await;
                        let body = body.to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        return;
                    }
                    let Ok(mut socket) = accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(text))) = socket.next().await {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        let event = json!({"method": "Runtime.consoleAPICalled", "params": {}});
                        let _ = socket.send(Message::text(event.to_string())).await;
                        match answer(&request) {
                            Some(reply) => {
                                let _ = socket.send(Message::text(reply)).await;
                            }
                            None => break,
                        }
                    }
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn evaluates_skips_events_and_reports_closed_sockets() {
        let port = devtools_server(json!([]), |request| {
            let expression = request["params"]["expression"].as_str().unwrap();
            (expression != "close").then(|| {
                json!({"id": request["id"], "result": {"result": {"type": "string", "value": expression}}})
                    .to_string()
            })
        })
        .await;
        let mut cdp = Cdp::connect(port, "ABCDEF12").await.unwrap();
        assert_eq!(cdp.evaluate("one").await.unwrap()["result"]["value"], "one");
        assert_eq!(cdp.evaluate("two").await.unwrap()["result"]["value"], "two");
        assert!(cdp.evaluate("close").await.is_err());
        assert!(cdp.evaluate("after").await.is_err());
    }

    #[tokio::test]
    async fn refuses_oversized_replies_and_protocol_errors() {
        let port = devtools_server(json!([]), |request| {
            Some(if request["params"]["expression"] == "big" {
                "x".repeat(MAX_READ + 1)
            } else {
                json!({"id": request["id"], "error": {"code": -32000}}).to_string()
            })
        })
        .await;
        let mut cdp = Cdp::connect(port, "ABCDEF12").await.unwrap();
        assert!(cdp.evaluate("error").await.is_err());
        assert!(cdp.evaluate("big").await.is_err());
    }
}
