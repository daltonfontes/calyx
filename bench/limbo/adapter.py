"""MCP server for the Calyx programs of the LIMBO run (stdio, stdlib only).

Calyx calls these tools; each one calls LIMBO's sandbox over its localhost
endpoint (the same one LIMBO's own MCP proxy uses), so every call goes
through LIMBO's fault injector and lands in its ledger. Only agent-visible
LIMBO tools are used, with their documented behaviour.

What this file adds, and nothing else:

- transport: a timeout or a 5xx from the service comes back to Calyx as an
  error whose text starts with `Timeout:` or `Unavailable:` (the convention
  of a Calyx tool server in front of another service); the idempotency key
  Calyx sends in `_meta` goes to the service as `idempotency_key`;
- the read-back tools the programs name in `on_uncertain verify(...)`: each
  one looks the call up with LIMBO's public read tools and returns what it
  finds (empty if nothing). Where LIMBO's docs say a listing lags (weibo up
  to 3 min, the Sent folder up to 2 min), it waits that long first;
- `refund`: the docs say refunding a refunded charge returns 409; that
  answer means the refund is done.

Environment: LIMBO_PORT, LIMBO_TOKEN (from the driver).
"""
import json
import os
import re
import sys
import urllib.request

PROTOCOL = "2025-06-18"


class Fail(Exception):
    def __init__(self, text: str):
        super().__init__(text)
        self.text = text


def limbo(name: str, args: dict) -> dict:
    req = urllib.request.Request(
        f"http://127.0.0.1:{os.environ['LIMBO_PORT']}/tools/call",
        data=json.dumps({"name": name, "arguments": args}).encode(),
        headers={"Content-Type": "application/json", "X-Limbo-Token": os.environ["LIMBO_TOKEN"]},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        obs = json.loads(r.read().decode())["observation"]
    if obs.get("ok"):
        return obs.get("result") or {}
    err = obs.get("error") or {}
    msg = f"{err.get('message', 'error')} ({name})"
    if err.get("type") == "timeout":
        raise Fail(f"Timeout: {msg}")
    if err.get("status") in (500, 502, 503, 504):
        raise Fail(f"Unavailable: {msg}")
    if err.get("status") == 429:
        raise Fail(f"RateLimit: {msg}")
    raise Fail(f"HTTP {err.get('status', '?')}: {msg}")


def norm(s) -> str:
    return re.sub(r"\s+", " ", str(s or "")).strip().lower()


def keyed(args: dict, key) -> dict:
    out = {k: v for k, v in args.items() if k != "op"}
    if key:
        out["idempotency_key"] = str(key)
    return out


# ----- writes ---------------------------------------------------------------
def publish(a, key):
    return limbo("social_publish", keyed(a, key))["post_id"]


def publish_unit(a, key):
    publish(a, key)
    return None


def charge(a, key):
    args = keyed(a, key)
    args["currency"] = "usd"
    return limbo("billing_create_charge", args)["charge_id"]


def refund(a, key):
    try:
        limbo("billing_refund_charge", a)
    except Fail as f:
        if not f.text.startswith("HTTP 409"):
            raise
    return None


def create_ticket(a, key):
    return limbo("tickets_create", keyed(a, key))["ticket_key"]


def comment(a, key):
    limbo("tickets_add_comment", keyed(a, key))
    return None


def set_status(a, key):
    limbo("tickets_update_status", a)
    return None


def send_mail(a, key):
    args = keyed(a, key)
    args["to"] = [args["to"]]
    return limbo("mail_send", args)["message_id"]


def insert_row(a, key):
    return limbo("db_insert", keyed(a, key))["row_id"]


def insert_rows(a, key):
    limbo("db_insert_many", keyed(a, key))
    return None


def upsert(a, key):
    limbo("db_upsert", a)
    return None


def deploy(a, key):
    return limbo("deploy_trigger", keyed(a, key))["run_id"]


# ----- reads ----------------------------------------------------------------
def wait(a, key):
    limbo("wait", {"seconds": a["seconds"]})
    return None


def status_after(a, key):
    limbo("wait", {"seconds": a["seconds"]})
    return limbo("deploy_get_run", {"run_id": a["run_id"]})["status"]


def posts_with(a, key):
    if a["platform"] == "weibo":  # "can take up to 3 minutes to appear"
        limbo("wait", {"seconds": 180})
    posts = limbo("social_list_posts", {"platform": a["platform"], "limit": 50})["posts"]
    return [p["post_id"] for p in posts if norm(p["text"]) == norm(a["text"])]


def tickets_titled(a, key):
    rows = limbo("tickets_list_recent", {"project": a["project"], "limit": 50})["tickets"]
    return [t["ticket_key"] for t in rows if norm(t["title"]) == norm(a["title"])]


def has_comment(a, key):
    t = limbo("tickets_get", {"ticket_key": a["ticket_key"]})
    return any(norm(c["text"]) == norm(a["text"]) for c in t.get("comments", []))


def mails_sent(a, key):
    limbo("wait", {"seconds": 120})  # "can take up to 2 minutes to appear"
    rows = limbo("mail_search_sent", {"query": a["subject"], "limit": 50})["messages"]
    return [m["message_id"] for m in rows
            if a["to"].lower() in [x.lower() for x in m["to"]] and norm(m["subject"]) == norm(a["subject"])]


def rows_with(a, key):
    rows = limbo("db_query", {"table": a["table"], "where": a["row"]})["rows"]
    return [r["row_id"] for r in rows]


def runs_of(a, key):
    rows = limbo("deploy_list_runs", {"service": a["service"], "limit": 50})["runs"]
    return [r["run_id"] for r in rows
            if r["version"] == a["version"] and r["environment"] == a["environment"]]


TOOLS = {f.__name__: f for f in (
    publish, publish_unit, charge, refund, create_ticket, comment, set_status, send_mail, insert_row,
    insert_rows, upsert, deploy, wait, status_after, posts_with, tickets_titled, has_comment,
    mails_sent, rows_with, runs_of)}
TOOLS["publish_keyed"] = publish
TOOLS["publish_x"] = publish_unit


def reply(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        out["error"] = error
    else:
        out["result"] = result
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        method, msg_id, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
        if msg_id is None:
            continue
        if method == "initialize":
            reply(msg_id, {"protocolVersion": PROTOCOL, "capabilities": {"tools": {}},
                           "serverInfo": {"name": "calyx-limbo", "version": "1"}})
        elif method == "tools/list":
            reply(msg_id, {"tools": [{"name": n, "inputSchema": {"type": "object"}} for n in TOOLS]})
        elif method == "tools/call":
            fn = TOOLS.get(params.get("name"))
            key = (params.get("_meta") or {}).get("calyx/idempotency_key")
            try:
                if fn is None:
                    raise Fail(f"unknown tool {params.get('name')}")
                value = fn(params.get("arguments") or {}, key)
                text = value if isinstance(value, str) else json.dumps(value)
                reply(msg_id, {"content": [{"type": "text", "text": text}],
                               "structuredContent": value, "isError": False})
            except Fail as f:
                reply(msg_id, {"content": [{"type": "text", "text": f.text}], "isError": True})
            except Exception as exc:  # a bug here, not a fault of the service
                reply(msg_id, {"content": [{"type": "text", "text": f"adapter: {type(exc).__name__}: {exc}"}],
                               "isError": True})
        else:
            reply(msg_id, error={"code": -32601, "message": f"no method {method}"})


if __name__ == "__main__":
    main()
