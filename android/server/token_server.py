#!/usr/bin/env python3
"""Token server for gdirect mobile.

Run it on a machine that can reach Google (for example a server abroad). It
keeps your Google refresh token and hands short-lived access tokens to the
app. Only tokens pass through here: files still travel between the phone and
Google directly.

    python3 token_server.py login     # once: sign in to Google
    python3 token_server.py serve     # then: answer the app

Settings come from the environment:

    GDIRECT_CLIENT_ID, GDIRECT_CLIENT_SECRET   your Google OAuth client (Desktop app)
    GDIRECT_ACCESS_KEY                         the secret the app must present
    GDIRECT_DATA      where the refresh token is kept (default ./data)
    GDIRECT_HOST      address to listen on (default 127.0.0.1)
    GDIRECT_PORT      port to listen on (default 8787)

Needs Python 3.8+ and nothing else.
"""
import base64
import hashlib
import hmac
import json
import os
import secrets
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

AUTH_URL = "https://accounts.google.com/o/oauth2/v2/auth"
TOKEN_URL = "https://oauth2.googleapis.com/token"
ABOUT_URL = "https://www.googleapis.com/drive/v3/about?fields=user(emailAddress)"
SCOPE = "https://www.googleapis.com/auth/drive"
# Google lets Desktop clients redirect to any loopback port. Nothing listens
# here: the address bar of the failed page carries the code we need.
REDIRECT = "http://127.0.0.1:53682/"


def env(name, default=None):
    value = os.environ.get(name, default)
    if value is None or value == "":
        sys.exit(f"{name} is not set")
    return value


def data_file():
    folder = os.environ.get("GDIRECT_DATA", "data")
    os.makedirs(folder, mode=0o700, exist_ok=True)
    return os.path.join(folder, "account.json")


def post_form(url, fields):
    body = urllib.parse.urlencode(fields).encode()
    request = urllib.request.Request(url, data=body)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        try:
            detail = json.load(error)
        except Exception:
            detail = {}
        raise RuntimeError(
            f"Google answered {error.code}: {detail.get('error', 'unknown')} "
            f"{detail.get('error_description', '')}".strip()
        ) from None


def email_of(access_token):
    request = urllib.request.Request(ABOUT_URL, headers={"Authorization": f"Bearer {access_token}"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)["user"]["emailAddress"]


def login():
    client_id, client_secret = env("GDIRECT_CLIENT_ID"), env("GDIRECT_CLIENT_SECRET")
    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    state = secrets.token_urlsafe(16)
    query = urllib.parse.urlencode({
        "client_id": client_id,
        "redirect_uri": REDIRECT,
        "response_type": "code",
        "scope": SCOPE,
        "access_type": "offline",
        "prompt": "consent",
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": state,
    })
    print("1. Open this address in a browser that can reach Google and sign in:\n")
    print(f"   {AUTH_URL}?{query}\n")
    print("2. The browser then goes to a page at 127.0.0.1 that fails to load. That is expected.")
    print("   Copy the whole address from the address bar and paste it here.\n")
    pasted = input("Address: ").strip()
    parsed = urllib.parse.parse_qs(urllib.parse.urlparse(pasted).query)
    if parsed.get("state", [""])[0] != state:
        sys.exit("That address does not belong to this sign-in. Run login again.")
    if "code" not in parsed:
        sys.exit(f"Google reported: {parsed.get('error', ['no code in the address'])[0]}")
    reply = post_form(TOKEN_URL, {
        "client_id": client_id,
        "client_secret": client_secret,
        "code": parsed["code"][0],
        "code_verifier": verifier,
        "grant_type": "authorization_code",
        "redirect_uri": REDIRECT,
    })
    if "refresh_token" not in reply:
        sys.exit("Google returned no refresh token. Remove the app's access in your Google account and try again.")
    email = email_of(reply["access_token"])
    path = data_file()
    with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as out:
        json.dump({"email": email, "refresh_token": reply["refresh_token"]}, out)
    print(f"\nSigned in as {email}. Saved to {path}.")


class Tokens:
    """Caches the access token until shortly before it expires."""

    def __init__(self):
        self.lock = threading.Lock()
        self.token = None
        self.email = None
        self.expires = 0.0

    def get(self):
        with self.lock:
            if self.token and time.time() < self.expires - 600:
                return self.token, int(self.expires - time.time())
            try:
                with open(data_file()) as source:
                    account = json.load(source)
            except FileNotFoundError:
                raise RuntimeError("not signed in on the server: run `token_server.py login`") from None
            reply = post_form(TOKEN_URL, {
                "client_id": env("GDIRECT_CLIENT_ID"),
                "client_secret": env("GDIRECT_CLIENT_SECRET"),
                "refresh_token": account["refresh_token"],
                "grant_type": "refresh_token",
            })
            self.token = reply["access_token"]
            self.expires = time.time() + int(reply.get("expires_in", 3600))
            self.email = account.get("email")
            return self.token, int(self.expires - time.time())


class Limiter:
    """Slows down guessing of the access key: 10 failures a minute per address."""

    def __init__(self):
        self.lock = threading.Lock()
        self.failures = {}

    def blocked(self, address):
        with self.lock:
            recent = [t for t in self.failures.get(address, []) if time.time() - t < 60]
            self.failures[address] = recent
            return len(recent) >= 10

    def failed(self, address):
        with self.lock:
            self.failures.setdefault(address, []).append(time.time())


def make_handler(key, tokens, limiter):
    class Handler(BaseHTTPRequestHandler):
        server_version = "gdirect-token-server"

        def reply(self, status, payload):
            body = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == "/health":
                return self.reply(200, {"ok": True})
            self.reply(404, {"error": "not found"})

        def do_POST(self):
            if self.path.rstrip("/") != "/token":
                return self.reply(404, {"error": "not found"})
            # Behind a reverse proxy the peer is the proxy; that still limits guessing.
            address = self.client_address[0]
            if limiter.blocked(address):
                return self.reply(429, {"error": "too many wrong keys; wait a minute"})
            given = self.headers.get("Authorization", "")
            given = given[7:] if given.lower().startswith("bearer ") else ""
            if not hmac.compare_digest(given.encode(), key.encode()):
                limiter.failed(address)
                return self.reply(401, {"error": "wrong access key"})
            try:
                token, remaining = tokens.get()
            except Exception as error:
                return self.reply(502, {"error": str(error)})
            self.reply(200, {"access_token": token, "expires_in": remaining, "email": tokens.email})

        def log_message(self, fmt, *args):
            # Never log headers or bodies; the request line and status are enough.
            sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    return Handler


def serve():
    key = env("GDIRECT_ACCESS_KEY")
    if len(key) < 16:
        sys.exit("GDIRECT_ACCESS_KEY must be at least 16 characters; try: python3 -c 'import secrets; print(secrets.token_urlsafe(32))'")
    env("GDIRECT_CLIENT_ID"), env("GDIRECT_CLIENT_SECRET")
    host = os.environ.get("GDIRECT_HOST", "127.0.0.1")
    port = int(os.environ.get("GDIRECT_PORT", "8787"))
    server = ThreadingHTTPServer((host, port), make_handler(key, Tokens(), Limiter()))
    print(f"listening on {host}:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "login":
        login()
    elif command == "serve":
        serve()
    else:
        sys.exit(__doc__)
