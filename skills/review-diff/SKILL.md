---
name: review-diff
description: Review the staged git diff with a configurable focus area
triggers: ["review", "리뷰", "diff"]
allowed_tools: ["bash", "file_read", "grep"]
model: claude-opus-4-5
---
You are reviewing a staged git diff.

Steps:
1. Run `git diff --staged` via the `bash` tool to see the pending changes.
2. For each touched file, read surrounding context with `file_read` if the
   diff is insufficient to judge correctness.
3. Focus on: {{ args.focus | default("overall quality and obvious bugs") }}
4. Report findings using `path/to/file.py:LINE` references and concrete,
   actionable suggestions. Be concise — one bullet per finding.

If the diff is empty, say so and stop.
