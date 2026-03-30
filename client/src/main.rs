use std::time::Duration;

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use nuts_protocol::{ClientMsg, ProxyMsg, ServiceDef};

/// nuts-client — tunnel local services through a nuts-proxy instance.
#[derive(Parser, Debug)]
#[command(name = "nuts-client", version)]
struct Cli {
    /// WebSocket URL of the nuts-proxy (e.g. wss://proxy.example.com/nuts/ws)
    #[arg(long, env = "NUTS_PROXY_URL", default_value = "")]
    proxy: String,

    /// Auth token (must match NUTS_TOKEN on the proxy)
    #[arg(long, env = "NUTS_TOKEN", default_value = "")]
    token: String,

    /// Services to expose, as subdomain=port pairs.
    /// Example: --service shivvr=8080 --service ocr=8888
    #[arg(long = "service", short = 's', value_parser = parse_service)]
    services: Vec<ServiceDef>,

    /// Path to a TOML config file (alternative to CLI args).
    /// If provided, --service flags are merged with config file services.
    #[arg(long, short = 'c')]
    config: Option<std::path::PathBuf>,

    /// Seconds between reconnect attempts (exponential backoff, max 60s)
    #[arg(long, default_value = "5")]
    reconnect_delay: u64,

    /// Bind address for local HTTP calls (default: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
}

fn parse_service(s: &str) -> Result<ServiceDef, String> {
    let (subdomain, port_str) = s
        .split_once('=')
        .ok_or_else(|| format!("expected subdomain=port, got '{s}'"))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("invalid port: '{port_str}'"))?;
    Ok(ServiceDef {
        subdomain: subdomain.to_string(),
        port,
        description: None,
    })
}

/// TOML config file format.
#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    proxy_url: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    services: Vec<ServiceEntry>,
}

#[derive(serde::Deserialize)]
struct ServiceEntry {
    subdomain: String,
    port: u16,
    #[serde(default)]
    description: Option<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nuts_client=info".into()),
        )
        .init();

    let mut cli = Cli::parse();

    // Merge config file if provided.
    if let Some(ref path) = cli.config {
        let contents = std::fs::read_to_string(path).expect("failed to read config file");
        let cfg: ConfigFile = toml::from_str(&contents).expect("invalid config TOML");

        if cli.proxy.is_empty() {
            if let Some(url) = cfg.proxy_url {
                cli.proxy = url;
            }
        }
        if cli.token.is_empty() {
            if let Some(tok) = cfg.token {
                cli.token = tok;
            }
        }
        for svc in cfg.services {
            cli.services.push(ServiceDef {
                subdomain: svc.subdomain,
                port: svc.port,
                description: svc.description,
            });
        }
    }

    if cli.proxy.is_empty() {
        error!("no proxy URL — use --proxy, NUTS_PROXY_URL, or set proxy_url in config file");
        std::process::exit(1);
    }

    if cli.services.is_empty() {
        error!("no services to tunnel — use --service or --config");
        std::process::exit(1);
    }

    info!(
        "tunneling {} services through {}",
        cli.services.len(),
        cli.proxy
    );
    for svc in &cli.services {
        info!("  {} → {}:{}", svc.subdomain, cli.bind, svc.port);
    }

    // Reconnect loop.
    let mut delay = Duration::from_secs(cli.reconnect_delay);
    let max_delay = Duration::from_secs(60);

    loop {
        match run_tunnel(&cli).await {
            Ok(()) => {
                info!("tunnel closed cleanly, reconnecting...");
                delay = Duration::from_secs(cli.reconnect_delay);
            }
            Err(e) => {
                error!("tunnel error: {e}, reconnecting in {delay:?}...");
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(max_delay);
    }
}

async fn run_tunnel(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(&cli.proxy).await?;
    info!("connected to {}", cli.proxy);

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Send Register.
    let register = ClientMsg::Register {
        token: cli.token.clone(),
        services: cli.services.clone(),
    };
    ws_tx
        .send(Message::Text(serde_json::to_string(&register)?.into()))
        .await?;

    // Wait for Registered ack.
    match ws_rx.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<ProxyMsg>(&text) {
            Ok(ProxyMsg::Registered { ok: true, .. }) => {
                info!("registered successfully");
            }
            Ok(ProxyMsg::Registered {
                ok: false, error, ..
            }) => {
                return Err(format!("registration rejected: {}", error.unwrap_or_default()).into());
            }
            other => {
                return Err(format!("unexpected response: {other:?}").into());
            }
        },
        other => {
            return Err(format!("bad handshake: {other:?}").into());
        }
    }

    // Build a subdomain → port map for fast lookup.
    let port_map: std::collections::HashMap<String, u16> = cli
        .services
        .iter()
        .map(|s| (s.subdomain.clone(), s.port))
        .collect();

    let bind = cli.bind.clone();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    // Wrap ws_tx in Arc<Mutex> so spawned tasks can send responses.
    let ws_tx = std::sync::Arc::new(tokio::sync::Mutex::new(ws_tx));

    // Main read loop.
    while let Some(frame) = ws_rx.next().await {
        let frame = frame?;
        let text = match frame {
            Message::Text(t) => t,
            Message::Close(_) => break,
            Message::Ping(d) => {
                ws_tx.lock().await.send(Message::Pong(d)).await?;
                continue;
            }
            _ => continue,
        };

        match serde_json::from_str::<ProxyMsg>(&text) {
            Ok(ProxyMsg::HttpRequest {
                request_id,
                subdomain,
                method,
                uri,
                headers,
                body,
            }) => {
                let port = match port_map.get(&subdomain) {
                    Some(p) => *p,
                    None => {
                        // No local port for this subdomain — 502 back.
                        let resp = ClientMsg::HttpResponse {
                            request_id,
                            status: 502,
                            headers: vec![],
                            body: b"unknown service".to_vec(),
                        };
                        ws_tx
                            .lock()
                            .await
                            .send(Message::Text(serde_json::to_string(&resp)?.into()))
                            .await?;
                        continue;
                    }
                };

                let http = http.clone();
                let bind = bind.clone();
                let ws_tx = ws_tx.clone();

                // Handle each request concurrently.
                tokio::spawn(async move {
                    let url = format!("http://{bind}:{port}{uri}");
                    let method: reqwest::Method = method.parse().unwrap_or(reqwest::Method::GET);

                    let mut req = http.request(method, &url);
                    for (k, v) in &headers {
                        // Skip hop-by-hop headers.
                        let k_lower = k.to_lowercase();
                        if matches!(
                            k_lower.as_str(),
                            "host" | "connection" | "upgrade" | "transfer-encoding"
                        ) {
                            continue;
                        }
                        req = req.header(k.as_str(), v.as_str());
                    }
                    req = req.body(body);

                    let resp = match req.send().await {
                        Ok(r) => {
                            let status = r.status().as_u16();
                            let headers: Vec<(String, String)> = r
                                .headers()
                                .iter()
                                .map(|(k, v)| {
                                    (k.to_string(), v.to_str().unwrap_or("").to_string())
                                })
                                .collect();
                            let body = r.bytes().await.unwrap_or_default().to_vec();
                            ClientMsg::HttpResponse {
                                request_id,
                                status,
                                headers,
                                body,
                            }
                        }
                        Err(e) => {
                            warn!("local request to {url} failed: {e}");
                            ClientMsg::HttpResponse {
                                request_id,
                                status: 502,
                                headers: vec![],
                                body: format!("upstream error: {e}").into_bytes(),
                            }
                        }
                    };

                    let text = serde_json::to_string(&resp).unwrap();
                    let _ = ws_tx.lock().await.send(Message::Text(text.into())).await;
                });
            }
            Ok(ProxyMsg::Ping) => {
                let pong = serde_json::to_string(&ClientMsg::Pong)?;
                ws_tx
                    .lock()
                    .await
                    .send(Message::Text(pong.into()))
                    .await?;
            }
            Ok(ProxyMsg::Registered { .. }) => {
                warn!("unexpected Registered message");
            }
            Err(e) => {
                warn!("bad frame from proxy: {e}");
            }
        }
    }

    Ok(())
}
