use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

pub struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    pub fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: Value::Null,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub api_key: Option<String>,
    pub body: Value,
}

pub fn serve(
    route: impl Fn(&str, &str, &Value) -> Reply + 'static,
    body: impl FnOnce(),
) -> Vec<Call> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    plugin_toolkit::capsink::with_cap_sink(
        Box::new(move |cap: &str, req: &str| {
            assert_eq!(cap, "http.request");
            let req: Value = serde_json::from_str(req).unwrap();
            let url = req["url"].as_str().unwrap_or_default();
            let path = url.trim_start_matches("http://immich.test").to_string();
            let method = req["method"].as_str().unwrap_or_default().to_string();
            let api_key = req["headers"].as_array().and_then(|hs| {
                hs.iter()
                    .find(|h| {
                        h[0].as_str()
                            .is_some_and(|k| k.eq_ignore_ascii_case("x-api-key"))
                    })
                    .and_then(|h| h[1].as_str().map(String::from))
            });
            let raw: Vec<u8> = req["body"]
                .as_array()
                .map(|b| {
                    b.iter()
                        .filter_map(|x| x.as_u64())
                        .map(|x| x as u8)
                        .collect()
                })
                .unwrap_or_default();
            let sent: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let reply = route(&method, &path, &sent);
            log.lock().unwrap().push(Call {
                method,
                path,
                api_key,
                body: sent,
            });
            let bytes = if reply.body.is_null() {
                Vec::new()
            } else {
                reply.body.to_string().into_bytes()
            };
            Ok(json!({"status": reply.status, "headers": [], "body": bytes}).to_string())
        }),
        body,
    );
    let out = seen.lock().unwrap().clone();
    out
}

pub fn block<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}
