---
name: explain-error
description: Paste an error message and get a plain-language explanation with fix suggestions
triggers: ["explain", "error", "에러"]
allowed_tools: ["bash", "file_read", "grep"]
model: ""
---
The user encountered an error. Your job:

1. Read the error message carefully.
2. Identify the root cause — not just the symptom.
3. If a file path or line number is mentioned, use `file_read` to look
   at the surrounding code.
4. If the error mentions a dependency, use `bash` to check versions
   (`pip show`, `cargo tree`, `npm ls`, etc.).
5. Explain the error in plain language (one paragraph, no jargon).
6. Suggest a concrete fix (show the code change if possible).

Error message:
{{ args.error | default("(paste your error here)") }}
