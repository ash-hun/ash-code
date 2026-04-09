---
name: summarize-file
description: Read a file and emit a structured summary
triggers: ["summarize", "요약"]
allowed_tools: ["file_read"]
---
Summarize the file at `{{ args.path | default("(unspecified)") }}`.

Produce output in this exact shape:

- **Purpose**: one sentence on what the file is for.
- **Key symbols**: top-level functions, classes, and constants with a
  one-line description each.
- **External dependencies**: third-party imports / linked resources.
- **Notable risks**: anything obviously brittle, unsafe, or TODO-ish.

Keep the whole summary under 20 lines. Quote file locations as
`{{ args.path }}:LINE`.
