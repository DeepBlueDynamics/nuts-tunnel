# nuts-tunnel

Reverse tunnel for exposing local services through a cloud proxy. No middlemen — you own both ends.

```
                Internet
                   │
            *.your-domain.com
                   │ HTTPS (auto-TLS)
                   ▼
          ┌─────────────────┐
          │   nuts-proxy     │  Cloud Run / any server
          │                  │
          │  routes by Host  │
          │  header through  │
          │  WebSocket tunnel│
          └────────┬─────────┘
                   │
              persistent WS
           (client dials out)
                   │
                   ▼
          ┌─────────────────┐
          │   nuts-client    │  your machine
          │                  │
          │  :8080 app       │
          │  :3000 api       │
          │  :5432 db        │
          └─────────────────┘
```

**nuts-proxy** accepts HTTPS from the internet and routes requests by subdomain through a WebSocket tunnel to **nuts-client**, which forwards them to your local services.

- Zero exposed ports on your machine
- Client initiates all connections (works behind NAT/firewalls)
- Subdomain-based routing (`app.your-domain.com` → `localhost:8080`)
- JSON protocol over WebSocket — simple, debuggable, extensible
- No third-party dependencies, no accounts, no SaaS

## Quick Start

### 1. Build

```bash
git clone https://github.com/DeepBlueDynamics/nuts-tunnel.git
cd nuts-tunnel
cargo build --release
```

Binaries land in `target/release/`:
- `nuts-proxy` (3.4 MB)
- `nuts-client` (7.4 MB)

### 2. Run the Proxy

On any public server or cloud platform:

```bash
NUTS_TOKEN=your-secret-token ./nuts-proxy
```

The proxy listens on `$PORT` (default `8080`). It exposes two things:
- `GET /nuts/ws` — WebSocket endpoint for client tunnels
- Everything else — reverse proxy, routes by `Host` header to the matching tunnel

### 3. Run the Client

On your local machine:

```bash
./nuts-client \
  --proxy wss://your-proxy.example.com/nuts/ws \
  --token your-secret-token \
  --service app=8080 \
  --service api=3000
```

Or use a config file:

```bash
./nuts-client --config nuts-tunnel.toml
```

Now `app.your-domain.com` hits `localhost:8080` and `api.your-domain.com` hits `localhost:3000`.

### 4. Config File

```toml
proxy_url = "wss://your-proxy.example.com/nuts/ws"
token = "your-secret-token"

[[services]]
subdomain = "app"
port = 8080

[[services]]
subdomain = "api"
port = 3000

[[services]]
subdomain = "grafana"
port = 3001
description = "monitoring dashboard"
```

See [`client/nuts-tunnel.toml.example`](client/nuts-tunnel.toml.example) for a full example.

## Deploy to Google Cloud Run

### Build and push the container

```bash
# From the repo root
gcloud builds submit --tag gcr.io/YOUR_PROJECT/nuts-proxy

# Deploy
gcloud run deploy nuts-proxy \
  --image gcr.io/YOUR_PROJECT/nuts-proxy \
  --set-env-vars NUTS_TOKEN=your-secret-token \
  --allow-unauthenticated \
  --region us-central1 \
  --timeout 3600 \
  --min-instances 1
```

> `--min-instances 1` keeps one instance warm so the WebSocket tunnel stays alive.
> `--timeout 3600` sets the max request duration to 1 hour (Cloud Run's limit for WebSocket connections).

### Map your domain

1. In Cloud Run console, add a custom domain mapping for `*.your-domain.com`
2. In your DNS provider, add a CNAME: `*.your-domain.com → ghs.googlehosted.com.`
3. Cloud Run provisions TLS certificates automatically

### Or use the Dockerfile directly

```bash
docker build -f proxy/Dockerfile -t nuts-proxy .
docker run -p 8080:8080 -e NUTS_TOKEN=secret nuts-proxy
```

## How nuts.services Uses It

[nuts.services](https://nuts.services) runs nuts-tunnel to expose local AI microservices through Cloud Run:

```toml
proxy_url = "wss://nuts-proxy-xxxx.run.app/nuts/ws"
token = "..."

[[services]]
subdomain = "shivvr"
port = 8080

[[services]]
subdomain = "ocr"
port = 8888

[[services]]
subdomain = "ferricula"
port = 8764

[[services]]
subdomain = "grubcrawler"
port = 6792

[[services]]
subdomain = "sdr"
port = 9090
```

This makes `shivvr.nuts.services`, `ocr.nuts.services`, etc. resolve to Cloud Run, which tunnels requests back to the home server. No VPN, no Cloudflare, no port forwarding.

## Architecture

### Protocol

Client and proxy communicate over WebSocket using JSON messages:

**Client → Proxy:**

| Message | Purpose |
|---------|---------|
| `Register` | Authenticate and declare which subdomains to route |
| `HttpResponse` | Return the result of a proxied request |
| `Pong` | Keepalive reply |

**Proxy → Client:**

| Message | Purpose |
|---------|---------|
| `Registered` | Acknowledge or reject registration |
| `HttpRequest` | Forward an incoming HTTP request |
| `Ping` | Keepalive (every 30s) |

Each `HttpRequest` carries a `request_id` (UUID). The client echoes it back in the `HttpResponse`. This multiplexes concurrent requests over a single WebSocket connection.

### Request Flow

```
1. Browser → HTTPS → Cloud Run → nuts-proxy
2. nuts-proxy reads Host header → extracts subdomain
3. Looks up subdomain in tunnel registry
4. Serializes HTTP request as JSON, sends through WebSocket
5. nuts-client receives, forwards to localhost:{port}
6. Local service responds
7. nuts-client serializes response, sends back through WebSocket
8. nuts-proxy reconstructs HTTP response, returns to browser
```

### Reconnection

The client automatically reconnects on disconnect with exponential backoff (default 5s → max 60s). Cloud Run kills WebSocket connections after 60 minutes — the client detects this and redials immediately.

## Project Structure

```
nuts-tunnel/
├── Cargo.toml              # workspace
├── protocol/
│   └── src/lib.rs          # shared message types
├── proxy/
│   ├── Dockerfile          # Cloud Run container
│   └── src/main.rs         # Axum reverse proxy + WS tunnel manager
└── client/
    ├── nuts-tunnel.toml.example
    └── src/main.rs          # WS tunnel client + local HTTP forwarder
```

## Environment Variables

| Variable | Component | Description |
|----------|-----------|-------------|
| `NUTS_TOKEN` | both | Shared secret for tunnel auth |
| `NUTS_PROXY_URL` | client | WebSocket URL of the proxy |
| `PORT` | proxy | Listen port (default `8080`) |
| `RUST_LOG` | both | Log level (`nuts_proxy=debug`, `nuts_client=info`) |

## Limits

- Request body: 32 MB max (configurable in proxy source)
- Request timeout: 30s per proxied request
- WebSocket lifetime: reconnects automatically after Cloud Run's 60-min limit
- Concurrent requests: limited by channel buffer (256 per tunnel)

## License

MIT
