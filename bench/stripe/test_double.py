"""A local stand-in for the few Stripe endpoints stripe_server.py uses, to
test the wiring of run_stripe.py without a key or network. It is not a
result: run_stripe.py writes nothing to bench/results/ when STRIPE_API_BASE
points here.

It honours `Idempotency-Key` on POST like Stripe does (same key, same
response), and applies every other POST.

    python bench/stripe/test_double.py 12111 &
    STRIPE_API_BASE=http://127.0.0.1:12111 STRIPE_API_KEY=sk_test_double \\
        python bench/stripe/run_stripe.py
"""
import json
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
DB = {"customers": {}, "pis": {}, "refunds": [], "txns": [], "keys": {}, "n": 0}


def new_id(prefix):
    DB["n"] += 1
    return f"{prefix}_{DB['n']}"


def meta(form):
    return {k[9:-1]: v for k, v in form.items() if k.startswith("metadata[")}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        parts = url.path.strip("/").split("/")
        with LOCK:
            if parts[:2] == ["v1", "payment_intents"]:
                pi = DB["pis"][parts[2]]
                out = dict(pi)
                refunded = sum(r["amount"] for r in DB["refunds"] if r["payment_intent"] == pi["id"])
                if "latest_charge" in q.get("expand[]", ""):
                    out["latest_charge"] = {"amount_refunded": refunded}
                return self.send(out)
            if parts[:2] == ["v1", "refunds"]:
                return self.send({"data": [r for r in DB["refunds"]
                                           if r["payment_intent"] == q.get("payment_intent")]})
            if parts[:2] == ["v1", "customers"] and parts[3:] == ["balance_transactions"]:
                return self.send({"data": [t for t in DB["txns"] if t["customer"] == parts[2]]})
        self.send({"error": "not found"}, 404)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        form = dict(urllib.parse.parse_qsl(self.rfile.read(n).decode()))
        key = self.headers.get("Idempotency-Key")
        parts = self.path.strip("/").split("/")
        with LOCK:
            if key and key in DB["keys"]:
                return self.send(DB["keys"][key])
            if parts == ["v1", "customers"]:
                out = {"id": new_id("cus")}
                DB["customers"][out["id"]] = out
            elif parts == ["v1", "payment_intents"]:
                out = {"id": new_id("pi"), "amount": int(form["amount"]), "customer": form["customer"],
                       "status": "succeeded"}
                DB["pis"][out["id"]] = out
            elif parts == ["v1", "refunds"]:
                out = {"id": new_id("re"), "payment_intent": form["payment_intent"],
                       "amount": int(form["amount"]), "metadata": meta(form)}
                DB["refunds"].append(out)
            elif parts[:2] == ["v1", "customers"] and parts[3:] == ["balance_transactions"]:
                out = {"id": new_id("cbtxn"), "customer": parts[2], "amount": int(form["amount"]),
                       "metadata": meta(form)}
                DB["txns"].append(out)
            else:
                return self.send({"error": "not found"}, 404)
            if key:
                DB["keys"][key] = out
        self.send(out)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 12111), H).serve_forever()
