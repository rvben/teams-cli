//! Optional photos already loaded by the selected Teams page. Never fetches a URL.
use super::cdp::Cdp;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use url::Url;

const MAX_IMAGE: usize = 256 * 1024;
const MAX_ENCODED_IMAGE: usize = MAX_IMAGE.div_ceil(3) * 4;
const MAX_TOTAL: usize = 1024 * 1024;
const MAX_RESOURCES: usize = 128;
const MAX_PHOTOS: usize = 16;
const BUDGET: Duration = Duration::from_millis(1000);

/// Photo problems, including a missing Page domain, never fail the chat read.
pub(super) async fn enrich(page: &mut Cdp, result: &mut Value, personal: bool) {
    let Some(raw) = result["result"]["value"].as_str() else {
        return;
    };
    let Ok(mut data) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let requested = identities(&data, personal);
    if requested.is_empty() || data.get("error").is_some() {
        return;
    }
    let Ok(Some(photos)) = tokio::time::timeout(BUDGET, collect(page, &requested, personal)).await
    else {
        return;
    };
    if photos.is_empty() {
        return;
    }
    attach(&mut data, &photos, personal);
    // Leave room for helper protocol JSON escaping as well as existing chat text.
    if let Ok(raw) = serde_json::to_string(&data)
        && raw.len() <= super::MAX_READ / 2
    {
        result["result"]["value"] = json!(raw);
    }
}

fn identities(data: &Value, personal: bool) -> BTreeSet<String> {
    data["items"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|chat| chat["members"].as_array().into_iter().flatten())
        .filter_map(|member| member["userId"].as_str().and_then(|id| mri(id, personal)))
        .take(512)
        .collect()
}

fn mri(id: &str, personal: bool) -> Option<String> {
    if personal {
        (id.starts_with("8:")
            && id.len() > 3
            && id.len() <= 256
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_:.-".contains(&b)))
        .then(|| id.to_owned())
    } else {
        let id = id.strip_prefix("8:orgid:").unwrap_or(id);
        (id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            }))
        .then(|| format!("8:orgid:{}", id.to_ascii_lowercase()))
    }
}

fn approved_origin(url: &Url, personal: bool) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && if personal {
            url.host_str() == Some("teams.live.com")
        } else {
            matches!(
                url.host_str(),
                Some("teams.microsoft.com" | "teams.cloud.microsoft")
            )
        }
}

fn decode_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn photo_identity(raw: &str, origin: &Url, personal: bool) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    if !approved_origin(&url, personal) || url.origin() != origin.origin() {
        return None;
    }
    let parts: Vec<_> = url.path_segments()?.collect();
    // Exact route observed in Personal Teams. Work photos are eligible only when
    // this same route carries a valid canonical orgid; no invented lookup route.
    if parts.len() != 6
        || parts[..4] != ["api", "mt", "beta", "users"]
        || parts[5] != "profilepicturev2"
    {
        return None;
    }
    mri(&decode_segment(parts[4])?, personal)
}

fn frame(tree: &Value, personal: bool) -> Option<(&str, Url)> {
    let frame = &tree["frameTree"]["frame"];
    let id = frame["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128)?;
    let mut url = Url::parse(frame["url"].as_str()?).ok()?;
    // App navigation fragments are allowed on the page, never on resource URLs.
    url.set_fragment(None);
    approved_origin(&url, personal).then_some((id, url))
}

async fn collect(
    page: &mut Cdp,
    requested: &BTreeSet<String>,
    personal: bool,
) -> Option<BTreeMap<String, String>> {
    page.request("Page.enable", json!({}), BUDGET).await.ok()?;
    let tree = page
        .request("Page.getResourceTree", json!({}), BUDGET)
        .await
        .ok()?;
    let (frame_id, origin) = frame(&tree, personal)?;
    let mut candidates: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    // Only the selected main frame; iframe images are never joined into this account.
    for resource in tree["frameTree"]["resources"]
        .as_array()?
        .iter()
        .take(MAX_RESOURCES)
    {
        if resource["type"] != "Image" {
            continue;
        }
        let Some(mime) = resource["mimeType"].as_str() else {
            continue;
        };
        if !matches!(
            mime,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) {
            continue;
        }
        let Some(raw) = resource["url"].as_str() else {
            continue;
        };
        let Some(id) = photo_identity(raw, &origin, personal).filter(|id| requested.contains(id))
        else {
            continue;
        };
        candidates
            .entry(id)
            .or_default()
            .insert(raw.to_owned(), mime.to_owned());
    }
    let mut photos = BTreeMap::new();
    let mut total = 0;
    for (id, urls) in candidates
        .into_iter()
        .filter(|(_, urls)| urls.len() == 1)
        .take(MAX_PHOTOS)
    {
        let (url, mime) = urls.into_iter().next()?;
        let Ok(body) = page
            .request(
                "Page.getResourceContent",
                json!({"frameId":frame_id,"url":url}),
                BUDGET,
            )
            .await
        else {
            continue;
        };
        let Some(data) = raster(&body, &mime) else {
            continue;
        };
        if total + data.len() > MAX_TOTAL {
            continue;
        }
        total += data.len();
        photos.insert(id, data);
    }
    // Discard photos if the page navigated during the bounded read. Chat data remains
    // usable and is returned without enrichment; photos never trigger a retry or fetch.
    let final_tree = page
        .request("Page.getResourceTree", json!({}), BUDGET)
        .await
        .ok()?;
    let (final_id, final_url) = frame(&final_tree, personal)?;
    if frame_id != final_id || origin != final_url {
        return None;
    }
    Some(photos)
}

fn raster(body: &Value, mime: &str) -> Option<String> {
    if body["base64Encoded"] != true {
        return None;
    }
    let encoded = body["content"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= MAX_ENCODED_IMAGE)?;
    let bytes = STANDARD.decode(encoded).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE {
        return None;
    }
    let valid = match mime {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    };
    valid.then(|| format!("data:{mime};base64,{encoded}"))
}

fn attach(data: &mut Value, photos: &BTreeMap<String, String>, personal: bool) {
    let mut total = 0;
    let viewer = data["me"].as_str().and_then(|id| mri(id, personal));
    let Some(chats) = data["items"].as_array_mut() else {
        return;
    };
    for chat in chats {
        let mut other = BTreeSet::new();
        if let Some(members) = chat["members"].as_array_mut() {
            for member in members {
                let Some(id) = member["userId"].as_str().and_then(|id| mri(id, personal)) else {
                    continue;
                };
                if Some(&id) != viewer.as_ref() {
                    other.insert(id.clone());
                }
                if let Some(photo) = photos
                    .get(&id)
                    .filter(|photo| total + photo.len() <= MAX_TOTAL)
                {
                    total += photo.len();
                    member["avatarData"] = json!(photo);
                }
            }
        }
        if chat["chatType"] != "oneOnOne" || viewer.is_none() {
            continue;
        }
        let selected = if chat["id"] == "48:notes" {
            viewer.as_ref()
        } else if other.len() == 1 {
            other.first()
        } else {
            None
        };
        if let Some(photo) = selected
            .and_then(|id| photos.get(id))
            .filter(|photo| total + photo.len() <= MAX_TOTAL)
        {
            total += photo.len();
            chat["avatarData"] = json!(photo);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::cdp::tests::devtools_server;
    use std::sync::{Arc, Mutex};

    const PHOTO: &str =
        "https://teams.live.com/api/mt/beta/users/8%3Alive%3Aother/profilepicturev2?size=48";
    const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 1, 2, 3];
    fn body(bytes: &[u8]) -> Value {
        json!({"content": STANDARD.encode(bytes), "base64Encoded": true})
    }
    fn chats() -> Value {
        json!({"me":"8:live:viewer","items":[
            {"id":"19:chat@thread.v2","chatType":"oneOnOne","members":[{"userId":"8:live:viewer"},{"userId":"8:live:other"}]},
            {"id":"48:notes","chatType":"oneOnOne","members":[{"userId":"8:live:viewer"}]},
            {"id":"19:group@thread.v2","chatType":"group","members":[{"userId":"8:live:viewer"},{"userId":"8:live:other"}]}
        ]})
    }
    fn evaluated(data: &Value) -> Value {
        json!({"result":{"type":"string","value":data.to_string()}})
    }
    fn resource(url: &str) -> Value {
        json!({"url":url,"mimeType":"image/jpeg","type":"Image"})
    }
    fn tree(resources: Value) -> Value {
        json!({"frameTree":{"frame":{"id":"main","url":"https://teams.live.com/v2/"},"resources":resources}})
    }

    #[test]
    fn accepts_only_exact_person_routes_and_account_origins() {
        let origin = Url::parse("https://teams.live.com/v2/").unwrap();
        assert_eq!(
            photo_identity(PHOTO, &origin, true).as_deref(),
            Some("8:live:other")
        );
        for url in [
            PHOTO.replace("https:", "http:"),
            PHOTO.replace("teams.live.com", "teams.live.com.evil.test"),
            PHOTO.replace("teams.live.com", "teams.microsoft.com"),
            PHOTO.replace("teams.live.com", "user@teams.live.com"),
            PHOTO.replace("teams.live.com", "teams.live.com:19222"),
            format!("{PHOTO}#fragment"),
            PHOTO.replace("profilepicturev2", "profilepicturev2/extra"),
            PHOTO.replace("8%3Alive%3Aother", "8%3Alive%3Aother%2Felse"),
            PHOTO.replace("8%3Alive%3Aother", "8%3Alive%3Aother%00"),
            PHOTO.replace("8%3Alive%3Aother", "8%ZZlive"),
            PHOTO.replace("api/mt/beta/users", "api/files/users"),
        ] {
            assert!(photo_identity(&url, &origin, true).is_none(), "{url}");
        }
        let work = "12345678-1234-1234-1234-123456789ABC";
        assert_eq!(
            mri(work, false).as_deref(),
            Some("8:orgid:12345678-1234-1234-1234-123456789abc")
        );
        assert!(mri("8:live:other", false).is_none());
        assert!(mri("8:orgid:not-a-guid", false).is_none());
    }

    #[test]
    fn validates_raster_mime_magic_base64_and_limits() {
        assert!(
            raster(&body(JPEG), "image/jpeg")
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        for (bytes, mime) in [
            (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
            (b"GIF89a".as_slice(), "image/gif"),
            (b"RIFFabcdWEBP".as_slice(), "image/webp"),
        ] {
            assert!(raster(&body(bytes), mime).is_some());
        }
        for (value, mime) in [
            (body(JPEG), "image/svg+xml"),
            (body(b"<svg/>"), "image/jpeg"),
            (body(JPEG), "image/png"),
            (json!({"content":"%%%","base64Encoded":true}), "image/jpeg"),
            (
                json!({"content":STANDARD.encode(JPEG),"base64Encoded":false}),
                "image/jpeg",
            ),
            (body(&[]), "image/jpeg"),
            (body(&vec![0; MAX_IMAGE + 1]), "image/jpeg"),
        ] {
            assert!(raster(&value, mime).is_none());
        }
    }

    #[test]
    fn chooses_other_member_self_viewer_and_keeps_group_identity() {
        let photos = BTreeMap::from([
            ("8:live:viewer".into(), "viewer-photo".into()),
            ("8:live:other".into(), "other-photo".into()),
        ]);
        let mut data = chats();
        attach(&mut data, &photos, true);
        assert_eq!(data["items"][0]["avatarData"], "other-photo");
        assert_eq!(data["items"][1]["avatarData"], "viewer-photo");
        assert!(data["items"][2].get("avatarData").is_none());
        assert_eq!(data["items"][2]["members"][1]["avatarData"], "other-photo");
        let mut data = chats();
        data["me"] = Value::Null;
        attach(&mut data, &photos, true);
        assert!(data["items"][0].get("avatarData").is_none());
        let mut data = chats();
        data["items"][0]["members"]
            .as_array_mut()
            .unwrap()
            .push(json!({"userId":"8:live:third"}));
        attach(&mut data, &photos, true);
        assert!(data["items"][0].get("avatarData").is_none());
    }

    #[test]
    fn bounds_all_copies_across_chats_and_members() {
        let photo = "x".repeat(MAX_ENCODED_IMAGE);
        let photos = BTreeMap::from([("8:live:other".into(), photo)]);
        let mut data = json!({"me":"8:live:viewer","items":(0..200).map(|_|json!({"chatType":"oneOnOne","members":[{"userId":"8:live:other"}]})).collect::<Vec<_>>()});
        attach(&mut data, &photos, true);
        let bytes: usize = data["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|chat| {
                chat["avatarData"].as_str().map_or(0, str::len)
                    + chat["members"][0]["avatarData"]
                        .as_str()
                        .map_or(0, str::len)
            })
            .sum();
        assert!(bytes > 0 && bytes <= MAX_TOTAL);
    }

    #[tokio::test]
    async fn reads_only_uniquely_associated_main_frame_resources_on_same_socket() {
        let methods = Arc::new(Mutex::new(Vec::new()));
        let seen = methods.clone();
        let port=devtools_server(json!([]),move |request| {
            let method=request["method"].as_str().unwrap();seen.lock().unwrap().push(method.to_owned());
            let answer=match method {
                "Page.enable"=>json!({}),
                "Page.getResourceTree"=>{
                    let mut value=tree(json!([resource(PHOTO),resource(&PHOTO.replace("8%3Alive%3Aother","8%3Alive%3Aunknown")),resource(&PHOTO.replace("teams.live.com","evil.test")),{"type":"Image","mimeType":"image/svg+xml","url":PHOTO}]));
                    value["frameTree"]["childFrames"]=json!([{"frame":{"id":"child","url":"https://teams.live.com/"},"resources":[resource(&PHOTO.replace("8%3Alive%3Aother","8%3Alive%3Aviewer"))]}]);value
                },
                "Page.getResourceContent"=>{assert_eq!(request["params"]["frameId"],"main");assert_eq!(request["params"]["url"],PHOTO);body(JPEG)},
                _=>panic!("unexpected method"),
            }; Some(json!({"id":request["id"],"result":answer}).to_string())
        }).await;
        let mut page = Cdp::connect(port, "ABCDEF12").await.unwrap();
        let mut result = evaluated(&chats());
        enrich(&mut page, &mut result, true).await;
        let data: Value =
            serde_json::from_str(result["result"]["value"].as_str().unwrap()).unwrap();
        assert!(
            data["items"][0]["avatarData"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        assert!(data["items"][1].get("avatarData").is_none());
        assert_eq!(
            *methods.lock().unwrap(),
            [
                "Page.enable",
                "Page.getResourceTree",
                "Page.getResourceContent",
                "Page.getResourceTree"
            ]
        );
    }

    #[tokio::test]
    async fn skips_ambiguous_resources_and_discards_after_navigation() {
        for navigation in [false, true] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = calls.clone();
            let port=devtools_server(json!([]),move |request| {
                let answer=match request["method"].as_str().unwrap() {
                    "Page.enable"=>json!({}),
                    "Page.getResourceTree"=>{
                        let n=count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                        if navigation && n>0 {json!({"frameTree":{"frame":{"id":"main","url":"https://evil.test/"},"resources":[]}})}
                        else if navigation {tree(json!([resource(PHOTO)]))}
                        else {tree(json!([resource(PHOTO),resource(&format!("{PHOTO}&variant=2"))]))}
                    },
                    "Page.getResourceContent"=>{assert!(navigation);body(JPEG)},
                    _=>panic!("unexpected method"),
                };Some(json!({"id":request["id"],"result":answer}).to_string())
            }).await;
            let mut page = Cdp::connect(port, "ABCDEF12").await.unwrap();
            let mut result = evaluated(&chats());
            let original = result.clone();
            enrich(&mut page, &mut result, true).await;
            assert_eq!(result, original);
        }
    }

    #[tokio::test]
    async fn photo_failure_is_nonfatal_and_the_socket_still_reads() {
        let port = devtools_server(json!([]), |request| {
            Some(
                if request["method"] == "Page.enable" {
                    json!({"id":request["id"],"error":{"code":-32000}})
                } else {
                    assert_eq!(request["method"], "Runtime.evaluate");
                    json!({"id":request["id"],"result":{"result":{"value":"still connected"}}})
                }
                .to_string(),
            )
        })
        .await;
        let mut page = Cdp::connect(port, "ABCDEF12").await.unwrap();
        let mut result = evaluated(&chats());
        let original = result.clone();
        enrich(&mut page, &mut result, true).await;
        assert_eq!(result, original);
        assert_eq!(
            page.evaluate("read").await.unwrap()["result"]["value"],
            "still connected"
        );
    }

    #[tokio::test]
    async fn chat_session_enriches_but_message_session_never_requests_photos() {
        use crate::desktop::{owner::tests::FakeSystem, read_request, session::Session};
        let methods = Arc::new(Mutex::new(Vec::new()));
        let seen = methods.clone();
        let port = devtools_server(
            json!([{"id":"ABCDEF12","type":"page","url":"https://teams.live.com/v2/"}]),
            move |request| {
                let method = request["method"].as_str().unwrap();
                seen.lock().unwrap().push(method.to_owned());
                let value = match method {
                    "Runtime.evaluate" => evaluated(&chats()),
                    "Page.enable" => json!({}),
                    "Page.getResourceTree" => tree(json!([resource(PHOTO)])),
                    "Page.getResourceContent" => body(JPEG),
                    _ => panic!("unexpected method"),
                };
                Some(json!({"id":request["id"],"result":value}).to_string())
            },
        )
        .await;
        let mut session = Session::new(FakeSystem::teams());
        let reply = session
            .read(&read_request(port, None, 5, None).unwrap())
            .await
            .unwrap();
        let data: Value =
            serde_json::from_str(reply["result"]["result"]["value"].as_str().unwrap()).unwrap();
        assert!(data["items"][0].get("avatarData").is_some());
        let reply = session
            .read(&read_request(port, Some("19:fixture@thread.v2"), 5, None).unwrap())
            .await
            .unwrap();
        let data: Value =
            serde_json::from_str(reply["result"]["result"]["value"].as_str().unwrap()).unwrap();
        assert!(data["items"][0].get("avatarData").is_none());
        assert_eq!(
            *methods.lock().unwrap(),
            [
                "Runtime.evaluate",
                "Page.enable",
                "Page.getResourceTree",
                "Page.getResourceContent",
                "Page.getResourceTree",
                "Runtime.evaluate"
            ]
        );
    }

    #[tokio::test]
    async fn bounded_photo_timeout_leaves_chats_and_next_read_intact() {
        let port = devtools_server(json!([]), |request| {
            Some(
                if request["method"] == "Page.enable" {
                    // A stale unrelated reply leaves the request unanswered.
                    json!({"id":0,"result":{}})
                } else {
                    json!({"id":request["id"],"result":{"result":{"value":"next read"}}})
                }
                .to_string(),
            )
        })
        .await;
        let mut page = Cdp::connect(port, "ABCDEF12").await.unwrap();
        let mut result = evaluated(&chats());
        let original = result.clone();
        let start = std::time::Instant::now();
        enrich(&mut page, &mut result, true).await;
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(result, original);
        assert_eq!(
            page.evaluate("read").await.unwrap()["result"]["value"],
            "next read"
        );
    }
}
