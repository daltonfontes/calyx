#!/usr/bin/env python3
"""A remote MCP server behind OAuth, with its own authorization server, for
compiler/calyx-cli/tests/oauth.rs. It checks what a real one checks: the
client is registered and the redirect is the registered one, PKCE (S256),
the `resource`, and refresh tokens are rotated (an old one is refused).

Prints `port N` when it listens. `/admin/expire` makes the access tokens
invalid (as if they expired early); `/admin/revoke` also forgets the refresh
tokens (as if the person revoked the app); `/admin/stats` counts token
requests.
"""
import base64
import hashlib
import json
import secrets
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

lock = threading.Lock()
clients = {}    # client_id -> redirect_uris
codes = {}      # code -> (client_id, redirect_uri, challenge, resource)
access = set()
refresh = {}    # refresh token -> client_id
stats = {"codes": 0, "refreshes": 0, "calls": 0}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def base(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def send(self, status, body=None, headers=()):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        for k, v in headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        url = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        b = self.base()
        if url.path == "/.well-known/oauth-protected-resource/mcp":
            return self.send(200, {"resource": b + "/mcp", "authorization_servers": [b + "/auth"]})
        if url.path == "/.well-known/oauth-authorization-server/auth":
            return self.send(200, {
                "issuer": b + "/auth",
                "authorization_endpoint": b + "/auth/authorize",
                "token_endpoint": b + "/auth/token",
                "registration_endpoint": b + "/auth/register",
                "code_challenge_methods_supported": ["S256"],
            })
        if url.path == "/auth/authorize":
            # The person approves at once.
            with lock:
                ok = (q.get("response_type") == "code"
                      and q.get("redirect_uri") in clients.get(q.get("client_id"), [])
                      and q.get("code_challenge_method") == "S256"
                      and q.get("resource") == b + "/mcp"
                      and q.get("state"))
                if not ok:
                    return self.send(400, {"error": "invalid_request", "got": q})
                code = secrets.token_urlsafe(16)
                codes[code] = (q["client_id"], q["redirect_uri"], q["code_challenge"], q["resource"])
            back = q["redirect_uri"] + "?" + urllib.parse.urlencode({"code": code, "state": q["state"]})
            self.send_response(302)
            self.send_header("Location", back)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if url.path == "/admin/expire":
            with lock:
                access.clear()
            return self.send(200, {})
        if url.path == "/admin/revoke":
            with lock:
                access.clear()
                refresh.clear()
            return self.send(200, {})
        if url.path == "/admin/stats":
            return self.send(200, stats)
        self.send(404, {"error": "not found"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n).decode()
        b = self.base()
        if self.path == "/auth/register":
            req = json.loads(raw)
            if req.get("token_endpoint_auth_method") != "none":
                return self.send(400, {"error": "invalid_client_metadata"})
            cid = "client-" + secrets.token_hex(4)
            with lock:
                clients[cid] = req["redirect_uris"]
            return self.send(201, {"client_id": cid, "redirect_uris": req["redirect_uris"]})
        if self.path == "/auth/token":
            f = dict(urllib.parse.parse_qsl(raw))
            with lock:
                if f.get("grant_type") == "authorization_code":
                    c = codes.pop(f.get("code"), None)
                    digest = hashlib.sha256(f.get("code_verifier", "").encode()).digest()
                    challenge = base64.urlsafe_b64encode(digest).rstrip(b"=").decode()
                    if (c is None or c[0] != f.get("client_id") or c[1] != f.get("redirect_uri")
                            or c[2] != challenge or c[3] != f.get("resource")):
                        return self.send(400, {"error": "invalid_grant"})
                    stats["codes"] += 1
                elif f.get("grant_type") == "refresh_token":
                    if refresh.pop(f.get("refresh_token"), None) != f.get("client_id"):
                        return self.send(400, {"error": "invalid_grant",
                                               "error_description": "unknown refresh token"})
                    stats["refreshes"] += 1
                else:
                    return self.send(400, {"error": "unsupported_grant_type"})
                a, r = "at-" + secrets.token_hex(8), "rt-" + secrets.token_hex(8)
                access.add(a)
                refresh[r] = f["client_id"]
            return self.send(200, {"access_token": a, "token_type": "Bearer",
                                   "expires_in": 3600, "refresh_token": r})
        if self.path == "/mcp":
            auth = self.headers.get("Authorization", "")
            with lock:
                valid = auth.startswith("Bearer ") and auth[7:] in access
            if not valid:
                meta = b + "/.well-known/oauth-protected-resource/mcp"
                return self.send(401, {"error": "invalid_token"},
                                 [("WWW-Authenticate", f'Bearer resource_metadata="{meta}"')])
            msg = json.loads(raw)
            if "id" not in msg:
                return self.send(202)
            m = msg.get("method")
            if m == "initialize":
                result = {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                          "capabilities": {"tools": {}},
                          "serverInfo": {"name": "oauth-test", "version": "0"}}
            elif m == "tools/list":
                result = {"tools": [{"name": "whoami", "inputSchema": {"type": "object",
                          "properties": {"who": {"type": "string"}}},
                          "annotations": {"readOnlyHint": True}}]}
            elif m == "tools/call":
                with lock:
                    stats["calls"] += 1
                who = msg["params"].get("arguments", {}).get("who", "")
                result = {"content": [{"type": "text", "text": f"hello {who}, from behind OAuth"}]}
            else:
                return self.send(200, {"jsonrpc": "2.0", "id": msg["id"],
                                       "error": {"code": -32601, "message": m}})
            return self.send(200, {"jsonrpc": "2.0", "id": msg["id"], "result": result})
        self.send(404, {"error": "not found"})


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(f"port {server.server_address[1]}", flush=True)
server.serve_forever()
