use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A service the client wants to expose through the tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    /// Subdomain prefix (e.g. "shivvr" → shivvr.nuts.services)
    pub subdomain: String,
    /// Local port on the client machine
    pub port: u16,
    /// Optional description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// -- base64 helpers for body bytes in JSON --

fn serialize_bytes<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&B64.encode(bytes))
}

fn deserialize_bytes<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    B64.decode(&s).map_err(serde::de::Error::custom)
}

/// Messages sent from the client to the proxy.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// First message after WebSocket connect. Registers routes.
    Register {
        token: String,
        services: Vec<ServiceDef>,
    },
    /// Response to a proxied HTTP request.
    HttpResponse {
        request_id: String,
        status: u16,
        headers: Vec<(String, String)>,
        #[serde(serialize_with = "serialize_bytes", deserialize_with = "deserialize_bytes")]
        body: Vec<u8>,
    },
    /// Keepalive reply.
    Pong,
}

/// Messages sent from the proxy to the client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProxyMsg {
    /// Acknowledge registration.
    Registered {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Forward an incoming HTTP request to the client's local service.
    HttpRequest {
        request_id: String,
        subdomain: String,
        method: String,
        uri: String,
        headers: Vec<(String, String)>,
        #[serde(serialize_with = "serialize_bytes", deserialize_with = "deserialize_bytes")]
        body: Vec<u8>,
    },
    /// Keepalive.
    Ping,
}
