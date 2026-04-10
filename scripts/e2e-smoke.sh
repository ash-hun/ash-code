#!/usr/bin/env bash
# ==========================================================================
# ash-code E2E smoke test
#
# Verifies the full stack in a running compose environment:
#   compose up -> health -> skills -> commands -> chat -> cancel -> watch -> DB
#
# Prerequisites:
#   - docker compose available
#   - .env configured with at least one LLM provider API key
#
# Usage:
#   # With local postgres:
#   docker compose --profile local-db up -d
#   ./scripts/e2e-smoke.sh
#
#   # With in-memory store (no postgres):
#   ASH_SESSION_STORE=memory docker compose up -d ash-code
#   ./scripts/e2e-smoke.sh
# ==========================================================================
set -euo pipefail

BASE_URL="${ASH_BASE_URL:-http://localhost:8080}"
PASS=0
FAIL=0
TESTS=()

# --- Helpers ---------------------------------------------------------------

pass() { PASS=$((PASS + 1)); TESTS+=("[PASS] $1"); echo "  [PASS] $1"; }
fail() { FAIL=$((FAIL + 1)); TESTS+=("[FAIL] $1: $2"); echo "  [FAIL] $1: $2"; }

check_status() {
    local label="$1" url="$2" method="${3:-GET}" body="${4:-}" expected="${5:-200}"
    local status
    if [[ -n "$body" ]]; then
        status=$(curl -s -o /dev/null -w "%{http_code}" -X "$method" "$url" \
            -H "Content-Type: application/json" -d "$body")
    else
        status=$(curl -s -o /dev/null -w "%{http_code}" -X "$method" "$url")
    fi
    if [[ "$status" == "$expected" ]]; then
        pass "$label (HTTP $status)"
    else
        fail "$label" "expected $expected, got $status"
    fi
}

check_json_field() {
    local label="$1" url="$2" field="$3" expected="$4"
    local value
    value=$(curl -s "$url" | python3 -c "import sys,json; print(json.load(sys.stdin)$field)" 2>/dev/null || echo "__error__")
    if [[ "$value" == "$expected" ]]; then
        pass "$label"
    else
        fail "$label" "expected '$expected', got '$value'"
    fi
}

echo "============================================================"
echo " ash-code E2E smoke test"
echo " target: $BASE_URL"
echo "============================================================"
echo ""

# --- Wait for service ------------------------------------------------------

echo "[1/7] Waiting for service..."
for i in $(seq 1 30); do
    if curl -s "$BASE_URL/v1/health" > /dev/null 2>&1; then
        break
    fi
    if [[ $i -eq 30 ]]; then
        echo "  FATAL: service not reachable at $BASE_URL after 30s"
        exit 1
    fi
    sleep 1
done
pass "service reachable"

# --- Health -----------------------------------------------------------------

echo ""
echo "[2/7] Health endpoint..."
check_json_field "health status=ok" "$BASE_URL/v1/health" "['status']" "ok"
check_json_field "health api_version=v1" "$BASE_URL/v1/health" "['api_version']" "v1"

# --- Skills -----------------------------------------------------------------

echo ""
echo "[3/7] Skills..."
check_status "GET /v1/skills" "$BASE_URL/v1/skills"
SKILL_COUNT=$(curl -s "$BASE_URL/v1/skills" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['skills']))" 2>/dev/null || echo "0")
if [[ "$SKILL_COUNT" -ge 1 ]]; then
    pass "skills loaded ($SKILL_COUNT)"
else
    fail "skills loaded" "expected >= 1, got $SKILL_COUNT"
fi

# --- Commands ---------------------------------------------------------------

echo ""
echo "[4/7] Commands..."
check_status "GET /v1/commands" "$BASE_URL/v1/commands"
CMD_COUNT=$(curl -s "$BASE_URL/v1/commands" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['commands']))" 2>/dev/null || echo "0")
if [[ "$CMD_COUNT" -ge 1 ]]; then
    pass "commands loaded ($CMD_COUNT)"
else
    fail "commands loaded" "expected >= 1, got $CMD_COUNT"
fi

# --- Chat + Session persistence --------------------------------------------

echo ""
echo "[5/7] Chat + session persistence..."
SESSION_ID="e2e-smoke-$(date +%s)"

# Run a chat turn
CHAT_RESP=$(curl -s -N -X POST "$BASE_URL/v1/chat" \
    -H "Content-Type: application/json" \
    -d "{\"session_id\":\"$SESSION_ID\",\"prompt\":\"Say exactly: e2e-ok\"}" \
    --max-time 30 2>/dev/null || echo "")
if echo "$CHAT_RESP" | grep -q "outcome"; then
    pass "chat turn completed"
else
    fail "chat turn" "no outcome event in response"
fi

# Verify session was persisted
sleep 1
SESSION_RESP=$(curl -s "$BASE_URL/v1/sessions/$SESSION_ID")
MSG_COUNT=$(echo "$SESSION_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['summary']['message_count'])" 2>/dev/null || echo "0")
if [[ "$MSG_COUNT" -ge 2 ]]; then
    pass "session persisted (messages=$MSG_COUNT)"
else
    fail "session persisted" "expected >= 2 messages, got $MSG_COUNT"
fi

# --- Cancel -----------------------------------------------------------------

echo ""
echo "[6/7] Cancel..."
# Cancel on a non-active session (should return ok=false)
CANCEL_RESP=$(curl -s -X POST "$BASE_URL/v1/sessions/$SESSION_ID/cancel" \
    -H "Content-Type: application/json")
CANCEL_MSG=$(echo "$CANCEL_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['message'])" 2>/dev/null || echo "")
if [[ "$CANCEL_MSG" == "no active turn" ]]; then
    pass "cancel on idle session returns 'no active turn'"
else
    fail "cancel idle" "expected 'no active turn', got '$CANCEL_MSG'"
fi

# --- Sessions CRUD ----------------------------------------------------------

echo ""
echo "[7/7] Sessions CRUD..."
check_status "GET /v1/sessions" "$BASE_URL/v1/sessions"
check_status "GET /v1/sessions/$SESSION_ID" "$BASE_URL/v1/sessions/$SESSION_ID"

# Delete the test session
DEL_RESP=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$BASE_URL/v1/sessions/$SESSION_ID")
if [[ "$DEL_RESP" == "200" ]]; then
    pass "DELETE /v1/sessions/$SESSION_ID"
else
    fail "DELETE session" "expected 200, got $DEL_RESP"
fi

# Verify deletion
DEL_CHECK=$(curl -s -o /dev/null -w "%{http_code}" "$BASE_URL/v1/sessions/$SESSION_ID")
if [[ "$DEL_CHECK" == "404" ]]; then
    pass "session gone after delete"
else
    fail "session gone" "expected 404, got $DEL_CHECK"
fi

# --- Summary ----------------------------------------------------------------

echo ""
echo "============================================================"
echo " Results: $PASS passed, $FAIL failed"
echo "============================================================"
for t in "${TESTS[@]}"; do echo "  $t"; done
echo ""

if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
