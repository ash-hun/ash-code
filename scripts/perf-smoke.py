#!/usr/bin/env python3
"""ash-code performance smoke test.

Runs N sequential chat turns against a running ash-code instance and
reports timing, throughput, and resource metrics.

Usage:
    python3 scripts/perf-smoke.py                  # 20 turns (default)
    python3 scripts/perf-smoke.py --turns 100       # 100 turns
    python3 scripts/perf-smoke.py --base-url http://host:8080

Prerequisites:
    - ash-code stack running (docker compose up -d)
    - At least one LLM provider configured
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import urllib.request
import urllib.error


def health_check(base_url: str) -> bool:
    try:
        resp = urllib.request.urlopen(f"{base_url}/v1/health", timeout=5)
        data = json.loads(resp.read())
        return data.get("status") == "ok"
    except Exception:
        return False


def run_turn(base_url: str, session_id: str, prompt: str) -> dict:
    """Run a single chat turn and return timing + token info."""
    payload = json.dumps({
        "session_id": session_id,
        "prompt": prompt,
    }).encode()

    req = urllib.request.Request(
        f"{base_url}/v1/chat",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )

    start = time.monotonic()
    first_token_time = None
    input_tokens = 0
    output_tokens = 0
    text_chunks = 0
    error = None

    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            for raw_line in resp:
                line = raw_line.decode("utf-8", errors="replace").strip()
                if not line or line.startswith(":"):
                    continue
                if line.startswith("data: "):
                    data_str = line[6:]
                    if data_str == "[DONE]":
                        break
                    try:
                        event = json.loads(data_str)
                    except json.JSONDecodeError:
                        continue
                    etype = event.get("type", "")
                    if etype == "text":
                        text_chunks += 1
                        if first_token_time is None:
                            first_token_time = time.monotonic()
                    elif etype == "finish":
                        input_tokens = event.get("input_tokens", 0)
                        output_tokens = event.get("output_tokens", 0)
                    elif etype == "error":
                        error = event.get("message", "unknown")
    except Exception as exc:
        error = str(exc)

    elapsed = time.monotonic() - start
    ttft = (first_token_time - start) if first_token_time else None

    return {
        "elapsed_s": round(elapsed, 3),
        "ttft_s": round(ttft, 3) if ttft else None,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "text_chunks": text_chunks,
        "error": error,
    }


def get_session_info(base_url: str, session_id: str) -> dict | None:
    try:
        resp = urllib.request.urlopen(
            f"{base_url}/v1/sessions/{session_id}", timeout=5
        )
        return json.loads(resp.read())
    except Exception:
        return None


def main():
    parser = argparse.ArgumentParser(description="ash-code performance smoke")
    parser.add_argument("--turns", type=int, default=20, help="Number of turns")
    parser.add_argument(
        "--base-url", default="http://localhost:8080", help="ash-code base URL"
    )
    parser.add_argument(
        "--prompt",
        default="Reply with exactly one word: ok",
        help="Prompt for each turn (keep short for speed)",
    )
    args = parser.parse_args()

    print("=" * 60)
    print(f" ash-code performance smoke — {args.turns} turns")
    print(f" target: {args.base_url}")
    print("=" * 60)

    # Health check
    if not health_check(args.base_url):
        print("\nFATAL: service not reachable. Is the stack running?")
        sys.exit(1)
    print("\n[ok] service reachable\n")

    session_id = f"perf-smoke-{int(time.time())}"
    results: list[dict] = []
    errors = 0

    for i in range(1, args.turns + 1):
        result = run_turn(args.base_url, session_id, args.prompt)
        results.append(result)
        if result["error"]:
            errors += 1
            status = f"ERROR: {result['error']}"
        else:
            status = (
                f"{result['elapsed_s']:.2f}s"
                f" (TTFT {result['ttft_s']:.2f}s)"
                f" in={result['input_tokens']}"
                f" out={result['output_tokens']}"
            )
        print(f"  turn {i:3d}/{args.turns}: {status}")

    # --- Aggregate stats ---
    ok_results = [r for r in results if not r["error"]]
    print("\n" + "=" * 60)
    print(" Results")
    print("=" * 60)
    print(f"  Total turns:  {len(results)}")
    print(f"  Succeeded:    {len(ok_results)}")
    print(f"  Failed:       {errors}")

    if ok_results:
        elapsed = [r["elapsed_s"] for r in ok_results]
        ttfts = [r["ttft_s"] for r in ok_results if r["ttft_s"] is not None]
        total_in = sum(r["input_tokens"] for r in ok_results)
        total_out = sum(r["output_tokens"] for r in ok_results)
        total_time = sum(elapsed)

        print(f"\n  Latency (s):")
        print(f"    min:    {min(elapsed):.3f}")
        print(f"    max:    {max(elapsed):.3f}")
        print(f"    mean:   {statistics.mean(elapsed):.3f}")
        print(f"    median: {statistics.median(elapsed):.3f}")
        if len(elapsed) > 1:
            print(f"    stdev:  {statistics.stdev(elapsed):.3f}")

        if ttfts:
            print(f"\n  Time to first token (s):")
            print(f"    min:    {min(ttfts):.3f}")
            print(f"    max:    {max(ttfts):.3f}")
            print(f"    mean:   {statistics.mean(ttfts):.3f}")

        print(f"\n  Tokens:")
        print(f"    total input:   {total_in:,}")
        print(f"    total output:  {total_out:,}")
        print(f"    total time:    {total_time:.1f}s")
        print(f"    throughput:    {total_out / total_time:.1f} output tok/s"
              if total_time > 0 else "")

    # Session size
    session = get_session_info(args.base_url, session_id)
    if session:
        msg_count = session.get("summary", {}).get("message_count", 0)
        print(f"\n  Session '{session_id}':")
        print(f"    messages stored: {msg_count}")

    # Cleanup
    try:
        req = urllib.request.Request(
            f"{args.base_url}/v1/sessions/{session_id}", method="DELETE"
        )
        urllib.request.urlopen(req, timeout=5)
        print(f"    cleaned up: yes")
    except Exception:
        print(f"    cleaned up: no (manual cleanup needed)")

    print()
    if errors > 0:
        sys.exit(1)


if __name__ == "__main__":
    main()
