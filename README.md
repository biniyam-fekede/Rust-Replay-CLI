# traffic-replayer

Replay HTTP traffic from NDJSON logs to a target service — with PII sanitization, concurrent dispatch, S3 streaming, and latency statistics.

## Features

- Stream NDJSON from **local file** or **S3** — line-by-line, bounded memory
- **Filter** by method, path prefix, or original status (repeatable, OR logic)
- **PII sanitization**: strip sensitive headers, redact query params and JSON body fields
- **Concurrent replay** via Semaphore (`--concurrency`, default 50)
- **Rate limiting** via `governor` (`--rate` req/s)
- **Latency percentiles** p50 / p95 / p99 via `hdrhistogram`
- **Dry-run**: full pipeline without sending any requests
- **Graceful SIGINT** with configurable drain timeout
- Exit codes: `0` success · `1` fatal · `2` interrupted

---

## Architecture & Design Patterns

```
src/
├── main.rs / lib.rs          # Entry point (thin) + library re-exports
├── cli.rs / config.rs        # clap args → validated Config
├── models.rs                 # ReplayRecord, SanitizedReplayRecord, ReplayOutcome, AggregatedStats
├── error.rs                  # TrafficReplayerError (thiserror)
│
├── filters.rs                # ── CHAIN OF RESPONSIBILITY + FACTORY ──
│   Filter trait              #   Pluggable predicate interface (OCP)
│   MethodFilter              #   HTTP method allow-list (OR semantics)
│   PathPrefixFilter          #   Path prefix allow-list (OR semantics)
│   StatusFilter              #   Original status allow-list (OR semantics)
│   FilterChain               #   Composes filters with AND logic
│   FilterFactory             #   Builds chain from Config
│
├── sanitize.rs               # ── STRATEGY + FACADE + FACTORY ──
│   Sanitizer trait           #   Marker; implement to add new strategies (OCP)
│   HeaderSanitizer           #   Strips sensitive headers
│   QuerySanitizer            #   Redacts query parameter values
│   BodySanitizer             #   Recursively redacts JSON body keys
│   RecordSanitizer           #   Facade: composes all three strategies
│   SanitizerFactory          #   Builds RecordSanitizer from Config
│
├── input/
│   mod.rs                    # ── FACTORY + OCP ──
│   RecordSource trait        #   Add new backends without touching existing ones
│   SourceFactory             #   Selects LocalSource or S3Source from Config
│   local.rs → LocalSource    #   Async line-by-line tokio::fs streaming
│   s3.rs   → S3Source        #   aws-sdk-s3 ByteStream line streaming
│
├── replay/
│   client.rs                 #   reqwest::Client builder with all policies
│   sender.rs                 # ── STRATEGY + FACTORY ──
│   RequestSender trait       #   Dispatch interface (add gRPC, WebSocket … OCP)
│   HttpSender                #   Real HTTP dispatch via reqwest
│   DryRunSender              #   No-op — never opens a socket
│   SenderFactory             #   Picks strategy from Config.dry_run
│   worker.rs                 #   dispatch() — thin façade over RequestSender
│   pipeline.rs               #   Semaphore loop + rate limiter + CancellationToken
│
├── stats.rs                  #   StatsAccumulator + hdrhistogram + mpsc aggregator
└── summary.rs                #   stdout summary + JSON stats writer
```

**Data flow:**

```
NDJSON source (LocalSource / S3Source)
    │  RecordSource::into_stream()  →  LineStream
    ▼
Parse → ReplayRecord          (bad lines: warn!, parse_errors++)
    ▼
FilterChain::matches()        (reject: filtered_count++)
    ▼
RecordSanitizer::sanitize()   (headers stripped, query/body redacted)
    ▼
RateLimiter::until_ready()    (if --rate set)
    ▼
Semaphore::acquire()          (bounded by --concurrency)
    ▼
tokio::spawn → RequestSender::send()  (HttpSender or DryRunSender)
    ▼
mpsc channel → StatsAccumulator → AggregatedStats
    ▼
stdout: summary  |  file: --output-stats JSON
```

---

## Extension Points (Open/Closed Principle)

| What to add | What to implement | Where to register |
|---|---|---|
| New input backend (stdin, HTTP, Kafka) | `RecordSource` trait | `SourceFactory::create` |
| New filter type (header, body-size, regex) | `Filter` trait | `FilterFactory::from_config` |
| New sanitization strategy | `Sanitizer` marker + a `sanitize()` method | `RecordSanitizer::new` |
| New dispatch strategy (gRPC, WS, throttled) | `RequestSender` trait | `SenderFactory::from_config` |

No existing code needs modification — only additions.

---

## NDJSON format

```json
{
  "method": "GET",
  "path": "/api/v1/users/123",
  "timestamp": "2025-10-10T14:32:11Z",
  "query": "?lang=en",
  "headers": {
    "accept": "application/json",
    "set-cookie": ["a=1", "b=2"]
  },
  "body": null,
  "body_encoding": "utf8",
  "content_type": "application/json",
  "status": 200
}
```

- `method` and `path` are required; everything else is optional.
- Header values: single string `"v"` or array `["v1","v2"]` — both normalized.
- `body_encoding: "base64"` — body contains base64-encoded bytes (binary payloads).
- `status` is the **recorded** original response — used for `--status` filtering only.

---

## CLI Flags

| Flag | Default | Description |
|---|---|---|
| `--base-url` | required | Target base URL |
| `--input-file` | — | Local NDJSON (mutually exclusive with S3) |
| `--s3-bucket` + `--s3-key` | — | S3 source |
| `--concurrency` | 50 | Max in-flight requests |
| `--timeout-ms` | 30000 | Total request timeout |
| `--connect-timeout-ms` | 5000 | TCP connect timeout |
| `--method` | — | Filter by method (repeatable, OR) |
| `--path-prefix` | — | Filter by path prefix (repeatable, OR) |
| `--status` | — | Filter by original status (repeatable, OR) |
| `--rate` | — | Max req/s |
| `--dry-run` | false | Parse/filter/sanitize; do NOT send |
| `--verbose` | false | Debug-level tracing to stderr |
| `--output-stats` | — | Write JSON stats to file (never stdout) |
| `--max-records` | — | Stop after N records |
| `--insecure-skip-tls-verify` | false | Disable TLS cert verification |
| `--follow-redirects` | false | Follow HTTP redirects (default: OFF) |
| `--aws-region` | — | AWS region (falls back to `AWS_DEFAULT_REGION`) |
| `--sanitize-passthrough-keys` | — | Comma-separated keys exempt from sanitization |
| `--drain-timeout-ms` | 5000 | Max wait for in-flight requests on SIGINT |

---

## Examples

```bash
# Local file
traffic-replayer --base-url http://localhost:8080 --input-file ./traffic.ndjson

# Filter: GET /api/v1 with original status 200
traffic-replayer --base-url http://localhost:8080 --input-file ./traffic.ndjson \
  --method GET --path-prefix /api/v1 --status 200

# Multi-prefix OR filter
traffic-replayer --base-url http://localhost:8080 --input-file ./traffic.ndjson \
  --path-prefix /api/v1 --path-prefix /health

# S3 with rate limit
traffic-replayer --base-url http://localhost:8080 \
  --s3-bucket my-bucket --s3-key logs/replay.ndjson \
  --concurrency 200 --rate 1000 --aws-region us-east-1

# Dry run
traffic-replayer --base-url http://localhost:8080 --input-file ./traffic.ndjson --dry-run

# Exempt 'key' and 'session_id' from sanitization
traffic-replayer --base-url http://localhost:8080 --input-file ./traffic.ndjson \
  --sanitize-passthrough-keys key,session_id --output-stats stats.json
```

---

## Sanitization

### Headers removed (exact, case-insensitive)
`authorization` · `cookie` · `set-cookie` · `x-api-key` · `proxy-authorization`

### Query params redacted (value → `[REDACTED]`)
`email` · `phone` · `token` · `session` · `ssn` · `password` · `auth` · `key`

### JSON body keys redacted (recursive, only when `content_type` contains `application/json`)
`password` · `token` · `authorization` · `email` · `phone` · `ssn` · `secret` · `session` · `cookie` · `api_key`

> **Note:** Matching is exact and case-insensitive. Fields like `key` (music API) or `session` (game session) will be redacted. Use `--sanitize-passthrough-keys` to exempt them.

---

## AWS Credentials

Standard credential chain (nothing hardcoded):
1. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`
2. `~/.aws/credentials` / `~/.aws/config`
3. IAM instance profile / IMDS

Region: `--aws-region` → `AWS_DEFAULT_REGION` → `AWS_REGION` → aws-config default chain.

---

## Output

| Stream | Content |
|---|---|
| `stderr` | Tracing logs (info by default; debug with `--verbose`) |
| `stdout` | Human-readable run summary |
| File (`--output-stats`) | Full JSON stats (never stdout) |

---

## Exit Codes

| Code | Meaning |
|---|---|
| `0` | Normal completion (non-2xx responses do not cause non-zero exit) |
| `1` | Fatal: bad config, can't open source, S3 auth failure |
| `2` | SIGINT — partial stats printed with `[INTERRUPTED]` label |

---

## Testing

```bash
cargo test            # all tests (unit + integration)
cargo test --lib      # unit tests only
cargo test --test integration_test   # integration tests only
```

**Unit coverage** (embedded in each module): CLI arg parsing, `ReplayRecord` deserialization (all fields, string/array headers, base64 body), filter OR/AND logic, all sanitization paths, stats counting/percentiles/clamping/error categorization, async aggregator.

**Integration coverage** (mockito): end-to-end local replay, parse error recovery, method/path-prefix/status filters, dry-run (zero requests), stats accuracy, `--output-stats` JSON validity, `--max-records` limit, cancellation flag, URL double-slash prevention, empty input, PII header stripping, combined filters.

---

## Known Limitations

- No retries — each request is attempted exactly once
- Redirects off by default — use `--follow-redirects`
- SIGINT only — SIGTERM not handled
- Base64 bodies are decoded but not inspected for PII

## Visual Dashboard

The `display/dashboard.html` page visualises stats from `--output-stats`. To run it locally:

```bash
cd display && python3 -m http.server 8000
```

Then open http://localhost:8000/dashboard.html in your browser. Drop a `stats.json` file or click "load demo data".

### Deploy to Vercel

1. Import the repo in [Vercel](https://vercel.com) (or connect your GitHub).
2. In **Project Settings → General → Root Directory**, set `display` (or `traffic-replayer/display` if deploying from a monorepo root).
3. Leave **Build Command** and **Output Directory** empty (static site).
4. Deploy. The root URL (`/`) will serve the dashboard.

## Future Improvements

- `--retry N` with exponential backoff
- Per-request NDJSON outcome log
- Response assertion rules
- SIGTERM handling
- `--timestamp-replay` at original inter-request timing
- Prometheus metrics endpoint
