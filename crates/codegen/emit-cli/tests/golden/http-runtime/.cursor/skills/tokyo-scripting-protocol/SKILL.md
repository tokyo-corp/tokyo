---
name: tokyo-scripting-protocol
description: Automates generated Tokyo CLIs through their stable process and JSON protocol. Use when writing scripts, subprocess calls, CI jobs, agent tool calls, parsing stdout or stderr, handling retryable errors, checking exit codes, or when the user mentions --output json, --no-input, or machine-readable output.
---
# Tokyo Scripting Protocol

## Invoke direct commands

Direct CLI commands are the scripting protocol; there is no separate batch envelope or generated SDK. Pass an argument array to avoid shell quoting:

```python
import json
import subprocess

result = subprocess.run(
    ["my-cli", "orders", "create", "--body", "-",
     "--output", "json", "--no-input"],
    input=json.dumps({"reference": "order_123"}),
    capture_output=True,
    text=True,
)

if result.returncode == 0:
    value = json.loads(result.stdout)
else:
    error = json.loads(result.stderr)["error"]
```

Always set `--output json` explicitly; output defaults are TTY-adaptive and some automation allocates a pseudo-terminal. Use `json-raw` only when compact JSON is specifically useful. `--no-input` guarantees that the call does not prompt.

## Keep channels separate

- Successful machine output is JSON on stdout.
- Diagnostics and one structured error envelope are on stderr.
- Streaming commands emit incremental output and require stream-aware consumption.
- Binary commands write raw bytes; do not decode them as JSON.

Errors exit nonzero and include `code`, `message`, `retryable`, and `hint`; HTTP-related errors may include `http_status`. Retry unchanged only when `retryable` is true.

Stable exit codes:

- `0`: success
- `1`: general or unmapped failure
- `2`: usage error
- `3`: not found
- `4`: authentication or permission failure
- `5`: conflict

## Discover before calling

Use `<cli> schema` for stable command IDs, then `<cli> schema --command <ID>` for exact invocation and body forms. Never scrape human help or table output for automation.

Never place secrets in argv. Use a profile, environment variables, or `--credential-file`.
