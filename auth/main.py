"""nuts-auth: FastAPI auth service for nuts-tunnel.

Endpoints:
  POST /nuts/auth/login    — send magic link (6-digit code via email)
  POST /nuts/auth/verify   — verify code, return JWT
  POST /nuts/auth/tokens   — create API token (ahp_ format)
  GET  /nuts/dashboard     — dashboard UI (behind JWT cookie)
  POST /api/validate       — internal: validate an ahp_ token (called by proxy)
"""

import hashlib
import os
import secrets
import time
from datetime import datetime, timezone

import httpx
import jwt
from fastapi import FastAPI, Request, Response
from fastapi.responses import HTMLResponse, JSONResponse
from google.cloud import firestore

app = FastAPI(title="nuts-auth")

# ── Config ──────────────────────────────────────────────────────────────

JWT_SECRET = os.environ.get("NUTS_JWT_SECRET", "")
JWT_ALGORITHM = "HS256"
JWT_EXPIRY_SECONDS = 7 * 24 * 3600  # 7 days

AGENTMAIL_API_KEY = os.environ.get("AGENTMAIL_API_KEY", "")
AGENTMAIL_BASE_URL = os.environ.get("AGENTMAIL_BASE_URL", "https://api.agentmail.to/v0")
AGENTMAIL_INBOX = os.environ.get("AGENTMAIL_INBOX", "impossiblewind387@agentmail.to")

PUBLIC_URL = os.environ.get("NUTS_PROXY_PUBLIC_URL", "https://tunnel.nuts.services")

MAGIC_CODE_EXPIRY = 15 * 60  # 15 minutes


def _ensure_jwt_secret():
    global JWT_SECRET
    if not JWT_SECRET:
        JWT_SECRET = secrets.token_hex(32)
        print("WARNING: NUTS_JWT_SECRET not set — generated ephemeral secret")


_ensure_jwt_secret()

# ── Firestore ───────────────────────────────────────────────────────────

db = firestore.Client()


def hash_token(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


# ── JWT helpers ─────────────────────────────────────────────────────────


def create_jwt(email: str) -> str:
    payload = {
        "sub": email,
        "iat": int(time.time()),
        "exp": int(time.time()) + JWT_EXPIRY_SECONDS,
    }
    return jwt.encode(payload, JWT_SECRET, algorithm=JWT_ALGORITHM)


def verify_jwt(token: str) -> dict | None:
    try:
        return jwt.decode(token, JWT_SECRET, algorithms=[JWT_ALGORITHM])
    except jwt.PyJWTError:
        return None


# ── AgentMail ───────────────────────────────────────────────────────────


async def send_magic_email(email: str, code: str):
    if not AGENTMAIL_API_KEY:
        print(f"AGENTMAIL_API_KEY not set — magic code for {email}: {code}")
        return

    async with httpx.AsyncClient() as client:
        await client.post(
            f"{AGENTMAIL_BASE_URL}/inboxes/{AGENTMAIL_INBOX}/messages",
            headers={"Authorization": f"Bearer {AGENTMAIL_API_KEY}"},
            json={
                "to": [email],
                "subject": "nuts-tunnel login code",
                "body": (
                    f"Your nuts-tunnel verification code is: {code}\n\n"
                    f"This code expires in 15 minutes.\n\n"
                    f"If you didn't request this, ignore this email."
                ),
            },
        )


# ── POST /nuts/auth/login ──────────────────────────────────────────────


@app.post("/nuts/auth/login")
async def login(request: Request):
    body = await request.json()
    email = body.get("email", "").strip().lower()
    if not email or "@" not in email or len(email) < 5:
        return JSONResponse({"ok": False, "message": "invalid email"}, status_code=400)

    code = f"{secrets.randbelow(10**6):06d}"
    doc_id = hash_token(f"{email}:{code}")

    db.collection("nuts_magic_tokens").document(doc_id).set({
        "email": email,
        "code": code,
        "created": datetime.now(timezone.utc),
        "used": False,
    })

    await send_magic_email(email, code)

    return {"ok": True, "message": f"verification code sent to {email}"}


# ── POST /nuts/auth/verify ─────────────────────────────────────────────


@app.post("/nuts/auth/verify")
async def verify(request: Request):
    body = await request.json()
    email = body.get("email", "").strip().lower()
    code = body.get("token", "").strip()

    if not email or not code:
        return JSONResponse({"ok": False, "message": "missing email or code"}, status_code=400)

    doc_id = hash_token(f"{email}:{code}")
    doc_ref = db.collection("nuts_magic_tokens").document(doc_id)
    doc = doc_ref.get()

    if not doc.exists:
        return JSONResponse({"ok": False, "message": "invalid code"}, status_code=401)

    data = doc.to_dict()
    if data.get("used"):
        return JSONResponse({"ok": False, "message": "code already used"}, status_code=401)

    created = data.get("created")
    if created:
        age = (datetime.now(timezone.utc) - created.replace(tzinfo=timezone.utc)).total_seconds()
        if age > MAGIC_CODE_EXPIRY:
            return JSONResponse({"ok": False, "message": "code expired"}, status_code=401)

    # Mark used
    doc_ref.update({"used": True})

    # Ensure user exists
    db.collection("nuts_users").document(email).set(
        {"email": email, "last_login": datetime.now(timezone.utc)},
        merge=True,
    )

    token = create_jwt(email)
    response = JSONResponse({"ok": True, "jwt": token})
    response.set_cookie(
        "nuts_jwt",
        token,
        httponly=True,
        secure=True,
        samesite="lax",
        max_age=JWT_EXPIRY_SECONDS,
    )
    return response


# ── POST /nuts/auth/tokens ─────────────────────────────────────────────


@app.post("/nuts/auth/tokens")
async def create_token(request: Request):
    # Requires JWT auth (cookie or Authorization header)
    claims = _get_jwt_claims(request)
    if not claims:
        return JSONResponse({"ok": False, "message": "unauthorized"}, status_code=401)

    email = claims["sub"]
    name = (await request.json()).get("name", "default")

    raw_token = "ahp_" + secrets.token_hex(32)
    token_hash = hash_token(raw_token)

    db.collection("nuts_api_tokens").document(token_hash).set({
        "email": email,
        "name": name,
        "hash": token_hash,
        "created": datetime.now(timezone.utc),
        "revoked": False,
    })

    return {"ok": True, "token": raw_token, "name": name}


# ── POST /api/validate (internal, called by Rust proxy) ────────────────


@app.post("/api/validate")
async def validate_token(request: Request):
    body = await request.json()
    token = body.get("token", "")

    if not token.startswith("ahp_"):
        return {"valid": False}

    token_hash = hash_token(token)
    doc = db.collection("nuts_api_tokens").document(token_hash).get()

    if not doc.exists:
        return {"valid": False}

    data = doc.to_dict()
    if data.get("revoked"):
        return {"valid": False}

    return {"valid": True, "email": data.get("email")}


# ── GET /nuts/dashboard ────────────────────────────────────────────────


@app.get("/nuts/dashboard")
async def dashboard(request: Request):
    return HTMLResponse(DASHBOARD_HTML)


# ── Helpers ─────────────────────────────────────────────────────────────


def _get_jwt_claims(request: Request) -> dict | None:
    # Try cookie first
    token = request.cookies.get("nuts_jwt")
    if not token:
        # Try Authorization header
        auth = request.headers.get("authorization", "")
        if auth.startswith("Bearer "):
            token = auth[7:]
    if not token:
        return None
    return verify_jwt(token)


# ── Dashboard HTML ──────────────────────────────────────────────────────

DASHBOARD_HTML = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>nuts-tunnel</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: system-ui, -apple-system, sans-serif; background: #0a0a0a; color: #e0e0e0; min-height: 100vh; display: flex; align-items: center; justify-content: center; }
  .card { background: #1a1a1a; border: 1px solid #333; border-radius: 12px; padding: 2rem; max-width: 420px; width: 90%; }
  h1 { font-size: 1.5rem; margin-bottom: 0.5rem; }
  .sub { color: #888; font-size: 0.9rem; margin-bottom: 1.5rem; }
  label { display: block; font-size: 0.85rem; color: #aaa; margin-bottom: 0.3rem; }
  input { width: 100%; padding: 0.6rem; background: #111; border: 1px solid #444; border-radius: 6px; color: #fff; font-size: 1rem; margin-bottom: 1rem; }
  input:focus { outline: none; border-color: #5b9; }
  button { width: 100%; padding: 0.7rem; background: #5b9; color: #000; border: none; border-radius: 6px; font-size: 1rem; font-weight: 600; cursor: pointer; }
  button:hover { background: #4a8; }
  .msg { padding: 0.6rem; border-radius: 6px; margin-bottom: 1rem; font-size: 0.9rem; }
  .msg.ok { background: #1a3a2a; color: #5b9; }
  .msg.err { background: #3a1a1a; color: #f55; }
  .token-box { background: #111; border: 1px solid #333; border-radius: 6px; padding: 0.8rem; word-break: break-all; font-family: monospace; font-size: 0.85rem; margin: 0.5rem 0; }
  .hidden { display: none; }
  .step { margin-bottom: 1rem; }
</style>
</head>
<body>
<div class="card">
  <h1>nuts-tunnel</h1>
  <p class="sub">sign in to get your API token</p>

  <div id="msg" class="msg hidden"></div>

  <!-- Step 1: Email -->
  <div id="step-email" class="step">
    <label for="email">email</label>
    <input type="email" id="email" placeholder="you@example.com" autofocus>
    <button onclick="doLogin()">send code</button>
  </div>

  <!-- Step 2: Verify code -->
  <div id="step-code" class="step hidden">
    <label for="code">verification code</label>
    <input type="text" id="code" placeholder="123456" maxlength="6" inputmode="numeric">
    <button onclick="doVerify()">verify</button>
  </div>

  <!-- Step 3: Authenticated -->
  <div id="step-auth" class="step hidden">
    <label for="token-name">token name</label>
    <input type="text" id="token-name" placeholder="my-server" value="default">
    <button onclick="doCreateToken()">create API token</button>
    <div id="token-result" class="hidden">
      <p style="margin-top:1rem; color:#aaa; font-size:0.85rem;">your token (save it now, shown once):</p>
      <div id="token-value" class="token-box"></div>
      <p style="color:#888; font-size:0.8rem; margin-top:0.5rem;">use with: <code>nuts-client --token ahp_...</code></p>
    </div>
  </div>
</div>

<script>
let userEmail = '';

function showMsg(text, ok) {
  const el = document.getElementById('msg');
  el.textContent = text;
  el.className = 'msg ' + (ok ? 'ok' : 'err');
  el.classList.remove('hidden');
}

async function doLogin() {
  const email = document.getElementById('email').value.trim();
  if (!email) return;
  userEmail = email;
  const r = await fetch('/nuts/auth/login', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({email})
  });
  const d = await r.json();
  if (d.ok) {
    showMsg(d.message, true);
    document.getElementById('step-email').classList.add('hidden');
    document.getElementById('step-code').classList.remove('hidden');
    document.getElementById('code').focus();
  } else {
    showMsg(d.message, false);
  }
}

async function doVerify() {
  const code = document.getElementById('code').value.trim();
  if (!code) return;
  const r = await fetch('/nuts/auth/verify', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({email: userEmail, token: code})
  });
  const d = await r.json();
  if (d.ok) {
    showMsg('authenticated', true);
    document.getElementById('step-code').classList.add('hidden');
    document.getElementById('step-auth').classList.remove('hidden');
  } else {
    showMsg(d.message, false);
  }
}

async function doCreateToken() {
  const name = document.getElementById('token-name').value.trim() || 'default';
  const r = await fetch('/nuts/auth/tokens', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({name})
  });
  const d = await r.json();
  if (d.ok) {
    document.getElementById('token-value').textContent = d.token;
    document.getElementById('token-result').classList.remove('hidden');
    showMsg('token created: ' + d.name, true);
  } else {
    showMsg(d.message, false);
  }
}

document.getElementById('email').addEventListener('keydown', e => { if (e.key === 'Enter') doLogin(); });
document.getElementById('code').addEventListener('keydown', e => { if (e.key === 'Enter') doVerify(); });
</script>
</body>
</html>
"""
