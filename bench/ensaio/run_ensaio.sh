#!/bin/sh
# Ensaio de produção (docs/operacao.md), com serviços de verdade:
#
#   1. a máquina A começa o atendimento: consulta o pedido no Stripe, o
#      Gemini propõe o reembolso, e a execução espera a aprovação;
#   2. uma aprovação sem assinatura é recusada;
#   3. o gerente aprova, com assinatura; o `calyx worker` da máquina B
#      retoma a execução e paga;
#   4. a máquina B "morre" (kill -9) depois de o Stripe aplicar o
#      reembolso e antes de a resposta chegar;
#   5. o worker toma a execução de novo e a termina.
#
# No fim, confere no Stripe: um reembolso e um crédito, nem mais nem menos.
#
#   CALYX=~/.calyx/bin/calyx CALYX_DATABASE_URL=postgres://... \
#   STRIPE_API_KEY=sk_test_... GEMINI_API_KEY=... sh bench/ensaio/run_ensaio.sh
#
# Só com chave de teste do Stripe (sk_test_): o servidor recusa as outras.
set -eu

CALYX="${CALYX:-calyx}"
: "${CALYX_DATABASE_URL:?o diário fica no PostgreSQL: defina CALYX_DATABASE_URL}"
: "${STRIPE_API_KEY:?defina STRIPE_API_KEY (sk_test_...)}"
: "${GEMINI_API_KEY:?defina GEMINI_API_KEY}"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
worker_pid=""
cleanup() {
    [ -n "$worker_pid" ] && kill "$worker_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

say() { printf '\n== %s\n' "$*"; }
fail() { printf 'ENSAIO FALHOU: %s\n' "$*" >&2; exit 1; }

# O programa e o servidor do Stripe num diretório só, com o calyx.toml.
mkdir -p "$work/prog" "$work/a" "$work/b" "$work/keys"
cp "$here/atendimento.clyx" "$work/prog/"
cp "$here/../stripe/stripe_server.py" "$work/prog/"
export CALYX_KEYS_DIR="$work/keys"
export CALYX_TELEMETRY_FILE="$work/telemetria.jsonl"
export STRIPE_SLOW=15
public="$("$CALYX" keys new gerente | grep -o 'ed25519:[0-9a-f]*')"
cat >"$work/prog/calyx.toml" <<EOF
[tools.get_order]
command = ["python3", "stripe_server.py"]
[tools.refund]
command = ["python3", "stripe_server.py"]
[tools.credit]
command = ["python3", "stripe_server.py"]
[tools.credit_given]
command = ["python3", "stripe_server.py"]

[prices."gemini-3.5-flash-lite"]
input = 0.10
output = 0.40

[senders]
gerente = "$public"

[messages.Approval]
senders = ["gerente"]
EOF

stripe() { (cd "$work/prog" && python3 -c "import stripe_server as s, sys; print($1)"); }
status() { (cd "$work/b" && "$CALYX" runs) | awk -v id="$1" '$1 == id { print $3 }'; }

say "versão: $("$CALYX" --version)"
order="$(stripe 's.probe_order({}, {})')"
request="ensaio-$(date +%s)__slowrefund__"
echo "pedido de teste no Stripe: $order (10 dólares)"

say "1. máquina A: o Gemini propõe, e a execução espera a aprovação"
set +e
(cd "$work/a" && "$CALYX" run "$work/prog/atendimento.clyx" --request "$request" \
    --order "$order" --message "A caixa chegou amassada e o produto riscado.") \
    2>"$work/a.err"
code=$?
set -e
cat "$work/a.err"
[ "$code" = 4 ] || fail "a execução devia esperar a aprovação (código 4), saiu com $code"
run="$(sed -n 's/^calyx: run //p' "$work/a.err" | head -1)"

say "2. uma aprovação sem assinatura"
if (cd "$work/b" && "$CALYX" deliver "$run" Approval '{"kind": "Approved"}'); then
    fail "uma aprovação sem assinatura foi aceita"
fi
[ "$(status "$run")" = waiting ] || fail "sem assinatura, a execução devia continuar esperando"
echo "recusada; a execução continua esperando"

say "3. o gerente aprova, assinado; o worker da máquina B retoma"
(cd "$work/b" && "$CALYX" deliver "$run" Approval '{"kind": "Approved"}' --as gerente)
(cd "$work/b" && exec "$CALYX" worker --every 1) 2>"$work/worker.err" &
worker_pid=$!

say "4. a máquina B morre depois de o Stripe aplicar o reembolso"
i=0
until [ "$(stripe "s.refunds_with({'order': '$order', 'request': '$request'}, {})")" -ge 1 ]; do
    i=$((i + 1)); [ "$i" -lt 120 ] || fail "o reembolso não chegou ao Stripe"
    sleep 1
done
pid="$(pgrep -f "resume $run" | head -1)"
[ -n "$pid" ] || fail "o processo que retoma a execução não está rodando"
kill -9 "$pid"
echo "reembolso aplicado no Stripe; processo $pid morto antes da resposta"

say "5. o worker toma a execução de novo e a termina"
i=0
until [ "$(status "$run")" = finished ]; do
    i=$((i + 1)); [ "$i" -lt 180 ] || fail "a execução não terminou (situação: $(status "$run"))"
    sleep 1
done
kill "$worker_pid"; wait "$worker_pid" 2>/dev/null || true; worker_pid=""
cat "$work/worker.err"
(cd "$work/b" && "$CALYX" replay "$run" --quiet) || fail "o replay falhou"

say "Conferência no Stripe"
refunds="$(stripe "s.refunds_with({'order': '$order', 'request': '$request'}, {})")"
credits="$(stripe "len(s.credits_for('$order', '$request'))")"
spans="$(grep -c resourceSpans "$CALYX_TELEMETRY_FILE" || true)"
echo "reembolsos: $refunds, créditos: $credits, envios de telemetria: $spans"
[ "$refunds" = 1 ] || fail "devia haver 1 reembolso, há $refunds"
[ "$credits" = 1 ] || fail "devia haver 1 crédito, há $credits"
echo "ENSAIO OK"
