#!/bin/sh
# The probe (`calyx check --tools --probe`) against Stripe in test mode,
# twice: the server as it is, and with STRIPE_DROP_KEY=1 (it stops
# forwarding the key but still claims idempotencyKeyHint: true).
# Needs STRIPE_API_KEY (sk_test_...) and a release build.
# Writes bench/results/stripe_probe.txt.
cd "$(dirname "$0")" || exit 2
out=../results/stripe_probe.txt
calyx=../../target/release/calyx
{
    echo "# $(date -u +%F) calyx check refund.clyx --probe, Stripe test mode"
    echo "## the server as it is"
    "$calyx" check refund.clyx --probe; echo "exit $?"
    echo "## STRIPE_DROP_KEY=1: the key is not forwarded"
    STRIPE_DROP_KEY=1 "$calyx" check refund.clyx --probe; echo "exit $?"
} > "$out" 2>&1
cat "$out"
