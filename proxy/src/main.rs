use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use uuid::Uuid;

use nuts_protocol::{ClientMsg, ProxyMsg, ServiceDef};

/// In-flight request waiting for a response from the client.
type PendingMap = DashMap<String, oneshot::Sender<ClientMsg>>;

/// Handle to a connected tunnel client.
struct TunnelHandle {
    /// Send proxy messages into the WebSocket writer task.
    tx: mpsc::Sender<ProxyMsg>,
    /// Map of request_id → oneshot sender for pending responses.
    pending: Arc<PendingMap>,
}

/// Global state: subdomain → tunnel handle.
struct AppState {
    tunnels: DashMap<String, Arc<TunnelHandle>>,
    token: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nuts_proxy=info".into()),
        )
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let token = std::env::var("NUTS_TOKEN").unwrap_or_else(|_| {
        warn!("NUTS_TOKEN not set — any client can register tunnels");
        String::new()
    });

    let state = Arc::new(AppState {
        tunnels: DashMap::new(),
        token,
    });

    let app = Router::new()
        .route("/nuts/ws", any(ws_handler))
        .fallback(proxy_handler)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("bind failed");

    info!("nuts-proxy listening on :{port}");
    axum::serve(listener, app).await.expect("serve failed");
}

// ── WebSocket tunnel endpoint ──────────────────────────────────────────

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_tunnel(socket, state))
}

async fn handle_tunnel(socket: WebSocket, state: Arc<AppState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // First message must be Register.
    let services: Vec<ServiceDef> = match ws_rx.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
            Ok(ClientMsg::Register { token, services }) => {
                if !state.token.is_empty() && token != state.token {
                    let msg = ProxyMsg::Registered {
                        ok: false,
                        error: Some("bad token".into()),
                    };
                    let _ = ws_tx
                        .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                        .await;
                    return;
                }
                let msg = ProxyMsg::Registered {
                    ok: true,
                    error: None,
                };
                if ws_tx
                    .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                    .await
                    .is_err()
                {
                    return;
                }
                info!(
                    "client registered {} services: {:?}",
                    services.len(),
                    services.iter().map(|s| &s.subdomain).collect::<Vec<_>>()
                );
                services
            }
            _ => {
                warn!("first message was not Register");
                return;
            }
        },
        _ => {
            warn!("tunnel connection failed before Register");
            return;
        }
    };

    // Set up tunnel handle.
    let (tx, mut proxy_rx) = mpsc::channel::<ProxyMsg>(256);
    let pending: Arc<PendingMap> = Arc::new(DashMap::new());
    let handle = Arc::new(TunnelHandle {
        tx,
        pending: pending.clone(),
    });

    // Register all subdomains.
    let subdomains: Vec<String> = services.iter().map(|s| s.subdomain.clone()).collect();
    for sd in &subdomains {
        state.tunnels.insert(sd.clone(), handle.clone());
    }

    // Writer task: forward ProxyMsg to WebSocket.
    let writer = tokio::spawn(async move {
        // Send pings on a timer.
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                msg = proxy_rx.recv() => {
                    match msg {
                        Some(m) => {
                            let text = serde_json::to_string(&m).unwrap();
                            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    let ping = serde_json::to_string(&ProxyMsg::Ping).unwrap();
                    if ws_tx.send(Message::Text(ping.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Reader loop: receive ClientMsg from WebSocket.
    while let Some(Ok(frame)) = ws_rx.next().await {
        let text = match frame {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        match serde_json::from_str::<ClientMsg>(&text) {
            Ok(ClientMsg::HttpResponse {
                request_id,
                status,
                headers,
                body,
            }) => {
                if let Some((_, sender)) = pending.remove(&request_id) {
                    let _ = sender.send(ClientMsg::HttpResponse {
                        request_id,
                        status,
                        headers,
                        body,
                    });
                }
            }
            Ok(ClientMsg::Pong) => {}
            Ok(ClientMsg::Register { .. }) => {
                warn!("duplicate Register ignored");
            }
            Err(e) => {
                warn!("bad frame from client: {e}");
            }
        }
    }

    // Cleanup.
    writer.abort();
    for sd in &subdomains {
        state.tunnels.remove(sd);
    }
    info!("tunnel disconnected, removed: {subdomains:?}");
}

// ── HTTP proxy handler (fallback) ──────────────────────────────────────

async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
) -> Response {
    // Path-based routing: /shivvr/health → service "shivvr", forwarded URI "/health"
    // Extract first path segment as service name, strip it from the forwarded URI.
    let path = req.uri().path();
    let (service_name, remainder) = match path.strip_prefix('/') {
        Some(rest) => match rest.split_once('/') {
            Some((svc, rem)) => (svc.to_string(), format!("/{rem}")),
            None if !rest.is_empty() => (rest.to_string(), "/".to_string()),
            _ => {
                return (StatusCode::BAD_REQUEST, "missing service name in path — use /service/...").into_response();
            }
        },
        _ => {
            return (StatusCode::BAD_REQUEST, "missing service name in path — use /service/...").into_response();
        }
    };

    // Preserve query string.
    let forwarded_uri = match req.uri().query() {
        Some(q) => format!("{remainder}?{q}"),
        None => remainder,
    };

    // Look up tunnel.
    let handle = match state.tunnels.get(&service_name) {
        Some(h) => h.clone(),
        None => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("no tunnel for '{service_name}'"),
            )
                .into_response();
        }
    };

    // Decompose the request.
    let method = req.method().to_string();
    let uri = forwarded_uri;
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let body = match axum::body::to_bytes(req.into_body(), 32 * 1024 * 1024).await {
        Ok(b) => b.to_vec(),
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "body too large (32MB max)").into_response();
        }
    };

    let request_id = Uuid::new_v4().to_string();

    // Set up response channel.
    let (resp_tx, resp_rx) = oneshot::channel();
    handle.pending.insert(request_id.clone(), resp_tx);

    // Send request to client via tunnel.
    let proxy_msg = ProxyMsg::HttpRequest {
        request_id: request_id.clone(),
        subdomain: service_name.clone(),
        method,
        uri,
        headers,
        body,
    };

    if handle.tx.send(proxy_msg).await.is_err() {
        handle.pending.remove(&request_id);
        return (StatusCode::BAD_GATEWAY, "tunnel send failed").into_response();
    }

    // Wait for response with timeout.
    match tokio::time::timeout(Duration::from_secs(30), resp_rx).await {
        Ok(Ok(ClientMsg::HttpResponse {
            status,
            headers,
            body,
            ..
        })) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut response_headers = HeaderMap::new();
            for (k, v) in &headers {
                if let (Ok(name), Ok(val)) = (
                    k.parse::<axum::http::HeaderName>(),
                    v.parse::<axum::http::HeaderValue>(),
                ) {
                    response_headers.insert(name, val);
                }
            }
            (status, response_headers, Bytes::from(body)).into_response()
        }
        Ok(Ok(_)) => (StatusCode::BAD_GATEWAY, "unexpected response type").into_response(),
        Ok(Err(_)) => (StatusCode::BAD_GATEWAY, "tunnel dropped").into_response(),
        Err(_) => {
            handle.pending.remove(&request_id);
            (StatusCode::GATEWAY_TIMEOUT, "request timed out (30s)").into_response()
        }
    }
}
