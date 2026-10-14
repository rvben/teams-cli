//! Build-pinned UI-free Teams startup. Unknown builds fail closed; no stale-cache fallback.
use super::cdp::Cdp;
use crate::error::AppError;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const REQUEST: Duration = Duration::from_secs(10);
const ENTRY_HASH: &str = "c6f7ec460f92ae83a80f52a06d86e051c0ca37016d6716efda82951cd904898a";
const RUNTIME_HASH: &str = "327994789b2c20e0c602e58bb00064532bd108a8398888073c6e221802b20601";
const BOOTSTRAP: &str = include_str!("../desktop_core.js");
const PRELOAD: &str = r#"(()=>{
 const descriptor=Object.getOwnPropertyDescriptor(HTMLScriptElement.prototype,'integrity'),original=Element.prototype.setAttribute;
 Object.defineProperty(HTMLScriptElement.prototype,'integrity',{configurable:true,get(){return ''},set(){}});
 Element.prototype.setAttribute=function(name,value){return original.call(this,name,name.toLowerCase()==='integrity'?'':value)};
 window.__teamsCliRestoreIntegrity=()=>{Object.defineProperty(HTMLScriptElement.prototype,'integrity',descriptor);Element.prototype.setAttribute=original;delete window.__teamsCliRestoreIntegrity};
})()"#;

fn failed() -> AppError {
    AppError::Desktop("browser-core startup is incompatible or incomplete; its browser was preserved. Disconnect it and use --desktop-client browser for the full web client".into())
}

#[derive(Clone, Copy)]
enum Asset {
    Entry,
    Runtime,
}

fn asset(raw: &str) -> Option<Asset> {
    let url = url::Url::parse(raw).ok()?;
    // Only known public assets, never sign-in endpoints or authenticated responses.
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if !matches!(
        url.host_str(),
        Some(
            "statics.teams.cdn.office.net"
                | "res.cdn.office.net"
                | "tflteams.public.onecdn.static.microsoft"
        )
    ) {
        return None;
    }
    let name = url.path_segments()?.next_back()?;
    if name.starts_with("async-entry-") && name.ends_with(".js") {
        Some(Asset::Entry)
    } else if name.starts_with("runtime-") && name.ends_with(".js") {
        Some(Asset::Runtime)
    } else {
        None
    }
}

fn replace_once(source: &str, from: &str, to: &str) -> Result<String, AppError> {
    if source.matches(from).count() != 1 {
        return Err(failed());
    }
    Ok(source.replacen(from, to, 1))
}

fn patch(source: &str, kind: Asset) -> Result<String, AppError> {
    let expected = match kind {
        Asset::Entry => ENTRY_HASH,
        Asset::Runtime => RUNTIME_HASH,
    };
    if format!("{:x}", Sha256::digest(source.as_bytes())) != expected {
        return Err(failed());
    }
    match kind {
        Asset::Entry => replace_once(
            source,
            "await d.init();const m=d.coreServices,",
            &format!(
                "await d.init();({BOOTSTRAP})(d,i,(client,context)=>YOe(client,context));return;const m=d.coreServices,"
            ),
        ),
        Asset::Runtime => replace_once(source, "a.m=_,", "a.m=_,a.ff=true,"),
    }
}

async fn paused(page: &mut Cdp, event: Value) -> Result<Option<Asset>, AppError> {
    let params = &event["params"];
    let id = params["requestId"].as_str().ok_or_else(failed)?;
    let Some(kind) = params["request"]["url"].as_str().and_then(asset) else {
        page.request("Fetch.continueResponse", json!({"requestId":id}), REQUEST)
            .await?;
        return Ok(None);
    };
    if params["responseStatusCode"] != 200 {
        return Err(failed());
    }
    let response = page
        .request("Fetch.getResponseBody", json!({"requestId":id}), REQUEST)
        .await?;
    let body = response["body"].as_str().ok_or_else(failed)?;
    let bytes = if response["base64Encoded"] == true {
        STANDARD.decode(body).map_err(|_| failed())?
    } else {
        body.as_bytes().to_vec()
    };
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(failed());
    }
    let source = String::from_utf8(bytes).map_err(|_| failed())?;
    let patched = patch(&source, kind)?;
    let headers: Vec<_> = params["responseHeaders"]
        .as_array()
        .ok_or_else(failed)?
        .iter()
        .filter(|h| {
            !matches!(
                h["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "content-length" | "content-encoding"
            )
        })
        .collect();
    page.request("Fetch.fulfillRequest", json!({"requestId":id,"responseCode":200,"responseHeaders":headers,"body":STANDARD.encode(patched)}), REQUEST).await?;
    Ok(Some(kind))
}

pub(super) async fn start(port: u16, target: &str) -> Result<(), AppError> {
    let mut page = Cdp::connect_bootstrap(port, target).await?;
    page.request("Network.enable", json!({}), REQUEST).await?;
    page.request("Page.enable", json!({}), REQUEST).await?;
    let preload = page
        .request(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":PRELOAD}),
            REQUEST,
        )
        .await?;
    page.request(
        "Network.setBypassServiceWorker",
        json!({"bypass":true}),
        REQUEST,
    )
    .await?;
    page.request(
        "Network.setCacheDisabled",
        json!({"cacheDisabled":true}),
        REQUEST,
    )
    .await?;
    page.request(
        "Storage.clearDataForOrigin",
        json!({"origin":"https://teams.live.com","storageTypes":"cache_storage"}),
        REQUEST,
    )
    .await?;
    page.request(
        "Fetch.enable",
        json!({"patterns":[{"urlPattern":"*async-entry-*.js*","requestStage":"Response"},{"urlPattern":"*runtime-*.js*","requestStage":"Response"}]}),
        REQUEST,
    )
    .await?;
    let result = async {
        page.request("Page.navigate", json!({"url":"https://teams.live.com/v2/"}), REQUEST).await?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let (mut entry, mut runtime) = (false, false);
        while Instant::now() < deadline {
            if let Some(event) = page.next_event(Duration::from_millis(250)).await? {
                match paused(&mut page, event).await? {
                    Some(Asset::Entry) => entry = true,
                    Some(Asset::Runtime) => runtime = true,
                    None => {}
                }
                continue;
            }
            if entry && runtime {
                // Start asynchronous readiness work without blocking paused requests.
                let status = page.evaluate("(()=>{const s=window.__teamsCliCore;if(!s)return false;if(!s.ready&&!s.pending)void s.prepare();return s.ready===true})()").await;
                if status.is_ok_and(|v| v["result"]["value"] == true) { return Ok(()); }
            }
        }
        Err(failed())
    }.await;
    // Always release paused responses, including unknown-build failures. Reads and
    // sends remain guarded by the ready marker and the persisted management phase.
    let cleanup = page.request("Fetch.disable", json!({}), REQUEST).await;
    let _ = page
        .request(
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({"identifier":preload["identifier"]}),
            REQUEST,
        )
        .await;
    let _ = page.evaluate("window.__teamsCliRestoreIntegrity?.()").await;
    if result.is_err() {
        let _ = page
            .request("Page.navigate", json!({"url":"about:blank"}), REQUEST)
            .await;
    }
    result?;
    cleanup?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_builds_and_ambiguous_anchors_fail_closed() {
        assert!(patch("await d.init();const m=d.coreServices,", Asset::Entry).is_err());
        assert!(patch("a.m=_,", Asset::Runtime).is_err());
        assert!(replace_once("x x", "x", "y").is_err());
        assert!(replace_once("z", "x", "y").is_err());
    }
    #[test]
    fn only_public_cdn_javascript_is_patchable() {
        assert!(asset("https://statics.teams.cdn.office.net/a/async-entry-123.js").is_some());
        for url in [
            "https://teams.live.com/api/async-entry-a.js",
            "http://res.cdn.office.net/runtime-a.js",
            "https://evil.example/runtime-a.js",
            "https://user@res.cdn.office.net/runtime-a.js",
            "https://res.cdn.office.net/not-runtime.js",
        ] {
            assert!(asset(url).is_none());
        }
    }
}
