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
  answer means the refund is done;
- `tools/list` passes on the MCP annotations LIMBO gives the tool each one
  calls (`readOnlyHint`, `idempotentHint`, ...), for `calyx check --tools`.

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


def post(path: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"http://127.0.0.1:{os.environ['LIMBO_PORT']}{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "X-Limbo-Token": os.environ["LIMBO_TOKEN"]},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode())


def limbo(name: str, args: dict) -> dict:
    obs = post("/tools/call", {"name": name, "arguments": args})["observation"]
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


def rows_present(a, key):
    """The rows of the batch already in the table (as they were asked for)."""
    return [r for r in a["rows"] if limbo("db_query", {"table": a["table"], "where": r})["rows"]]


def runs_of(a, key):
    rows = limbo("deploy_list_runs", {"service": a["service"], "limit": 50})["runs"]
    return [r["run_id"] for r in rows
            if r["version"] == a["version"] and r["environment"] == a["environment"]]


TOOLS = {f.__name__: f for f in (
    publish, charge, refund, create_ticket, comment, set_status, send_mail, insert_row,
    insert_rows, upsert, deploy, wait, status_after, posts_with, tickets_titled, has_comment,
    mails_sent, rows_with, rows_present, runs_of)}
TOOLS["publish_keyed"] = publish
TOOLS["publish_x"] = publish

# Under the native contract, the tools whose service honours an idempotency key.
HONORS_KEY = {"publish_keyed", "charge"}

# The LIMBO tool each one calls: its MCP annotations are passed on as theirs.
WRAPS = {
    "publish": "social_publish", "publish_keyed": "social_publish", "publish_x": "social_publish",
    "charge": "billing_create_charge", "refund": "billing_refund_charge", "create_ticket": "tickets_create",
    "comment": "tickets_add_comment", "set_status": "tickets_update_status", "send_mail": "mail_send",
    "insert_row": "db_insert", "insert_rows": "db_insert_many", "upsert": "db_upsert", "deploy": "deploy_trigger",
    "wait": "wait", "status_after": "deploy_get_run", "posts_with": "social_list_posts",
    "tickets_titled": "tickets_list_recent", "has_comment": "tickets_get", "mails_sent": "mail_search_sent",
    "rows_with": "db_query", "rows_present": "db_query", "runs_of": "deploy_list_runs",
}


def tool_list() -> list:
    """Every tool, with LIMBO's annotations where this episode's task has the tool it calls."""
    try:
        hints = {t["name"]: t.get("annotations") for t in post("/tools/list", {})["tools"]}
    except Exception:
        hints = {}
    out = []
    for name in TOOLS:
        entry = {"name": name, "inputSchema": {"type": "object"}}
        if hints.get(WRAPS.get(name)) is not None:
            entry["annotations"] = dict(hints[WRAPS[name]])
            # The proposed `idempotencyKeyHint` (docs/mcp/idempotency-key-hint.md),
            # declared as a server author would: from which services behind it
            # honour a key. Off by default, so earlier results reproduce.
            if os.environ.get("LIMBO_KEY_HINT") == "1" and not entry["annotations"].get("readOnlyHint"):
                entry["annotations"]["idempotencyKeyHint"] = name in HONORS_KEY
        out.append(entry)
    return out


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
            reply(msg_id, {"tools": tool_list()})
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
