# Tools

OpenCrust's agent runtime gives the LLM access to built-in tools and dynamically registered MCP tools. When the LLM decides to use a tool, the runtime executes it and feeds the result back into the conversation.

## Tool Loop

The agent processes each message through a tool loop that runs for up to **10 iterations**. In each iteration:

1. The LLM generates a response, optionally including tool calls
2. If tool calls are present, the runtime executes them
3. Tool results are appended to the conversation and sent back to the LLM
4. The loop continues until the LLM responds without tool calls or the iteration limit is reached

## Built-in Tools

### bash

Execute shell commands. Uses `bash -c` on Unix and `powershell -Command` on Windows.

| Property | Value |
|----------|-------|
| Timeout | 30 seconds |
| Max output | 32 KB (truncated if exceeded) |

**Input:**

```json
{ "command": "ls -la /tmp" }
```

Both stdout and stderr are captured. Stderr is prefixed with `STDERR:` in the output. Non-zero exit codes are reported as errors.

### file_read

Read the contents of a file.

| Property | Value |
|----------|-------|
| Max file size | 1 MB |
| Path traversal | Rejected (`..` components blocked) |

**Input:**

```json
{ "path": "/home/user/notes.txt" }
```

Files exceeding the size limit return an error rather than truncating.

### file_write

Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Parent directories are created automatically.

| Property | Value |
|----------|-------|
| Max content size | 1 MB |
| Path traversal | Rejected (`..` components blocked) |

**Input:**

```json
{ "path": "/home/user/output.txt", "content": "Hello, world!" }
```

### web_fetch

Fetch the content of a web page. Returns the raw response body (HTML, JSON, plain text).

| Property | Value |
|----------|-------|
| Timeout | 30 seconds |
| Max response size | 1 MB (truncated if exceeded) |

**Input:**

```json
{ "url": "https://example.com" }
```

Non-2xx HTTP status codes are reported as errors. An optional `blocked_domains` list can be configured to restrict which domains the agent can access.

### web_search

Search the web using the Brave Search API. Only available when a Brave API key is configured.

| Property | Value |
|----------|-------|
| Timeout | 15 seconds |
| Default results | 5 |
| Max results | 10 |
| Requires | `BRAVE_API_KEY` |

**Input:**

```json
{ "query": "rust async runtime comparison", "count": 5 }
```

The `count` parameter is optional (defaults to 5, clamped to 1-10). Results are returned as formatted markdown with title, snippet, and URL for each result.

The tool is only registered at startup if a Brave API key is found (config key `brave` or env var `BRAVE_API_KEY`).

#### Other providers

Set `tools.web_search.provider` in `config.yml` to pick another backend. The tool name, input and output format stay the same.

- `google`: Google Custom Search JSON API. Needs `api_key` and `search_engine_id` (or `GOOGLE_SEARCH_KEY` / `GOOGLE_SEARCH_ENGINE_ID`). Google says this API is closed to new customers and will be discontinued on January 1, 2027 ([overview](https://developers.google.com/custom-search/v1/overview)).
- `keenable`: [Keenable Search API](https://docs.keenable.ai/api-reference/search). Works without an API key (1,000 requests per hour per IP, see [rate limits](https://docs.keenable.ai/rate-limits)). An optional `api_key` (or `KEENABLE_API_KEY`) removes the hourly cap.

```yaml
tools:
  web_search:
    provider: keenable
    # api_key: keen_...   # optional
```

### schedule_heartbeat

Schedule a future wake-up for the agent. Useful for reminders, follow-ups, or checking back on long-running tasks.

| Property | Value |
|----------|-------|
| Max delay | 30 days (2,592,000 seconds) |
| Max pending per session | 5 |

**Input:**

```json
{ "delay_seconds": 3600, "reason": "Check if the deployment finished" }
```

The delay must be a positive integer. Heartbeats cannot be scheduled from within a heartbeat execution context (no recursive self-scheduling). The scheduled task is stored in SQLite and the scheduler polls for due tasks.

## MCP Tools

In addition to built-in tools, the agent can use tools from connected [MCP servers](./mcp.md). MCP tools are discovered at startup and registered with namespaced names in the format `server.tool_name`.

For example, a filesystem MCP server named `fs` exposing a `read_file` tool would appear as `fs.read_file`.

MCP tools have the same interface as built-in tools from the LLM's perspective - they receive JSON input and return text output.

See [MCP](./mcp.md) for configuration details.
