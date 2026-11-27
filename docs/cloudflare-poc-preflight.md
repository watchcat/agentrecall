# AgentRecall Cloudflare proof-of-concept preflight

Observed 26 November 2026, repository HEAD `31e715e`, workspace `/Users/watchcat/work/rust/agentrecall`.

Docker Desktop is installed and its daemon responds when the terminal operation is granted network access for its Unix socket. Client/engine version 29.9.2; host daemon is Linux ARM64. Cloudflare Containers require Linux AMD64 images, so the container proof must explicitly build/test `linux/amd64`, not assume the host architecture matches. [1]

Read-only Cloudflare checks were made against account `6c27fb90376881b1dd83114b986f17ab`. Listing R2 buckets whose names contain `agentrecall` succeeded and returned no matches; this is not proof that R2 data-plane credentials are available. Listing Containers applications returned HTTP 401 with the product error: `You do not have access to Cloudflare Containers. Deploying containers requires the Workers Paid plan.` Listing subscriptions returned HTTP 403 for missing Billing Read/Write scope; the actual subscription list could not be inspected. No subscription, bucket, Worker, secret, image, or container has been created or changed.

## Object-storage extension setup facts

The source repository is `https://github.com/dentiny/duckdb-object-storage`. Its README documents `duckdb_objfs://` for logical DuckDB database paths, SlateDB-managed physical layout, and local/memory/S3-compatible backends. [2] The extension catalog lists DuckDB versions including 2.0.1, but artifact availability for Linux AMD64 and this exact version must be verified experimentally rather than inferred from text-only rendering of the compatibility matrix. [3]

Configure settings before the first `duckdb_objfs://` access. Changing them afterwards fails; restart to change configuration. For S3, load `cache_httpfs` to register the `TYPE S3` secret, set `duckdb_objfs_backend='s3'`, bucket and root prefix, and use a scope-matching secret. Explicit secret options include `KEY_ID`, `SECRET`, `REGION`, `ENDPOINT`, `USE_SSL`, and `URL_STYLE`. Credentials can be read with `getenv(...)`; do not print credential-bearing SQL or secret diagnostics. For local MinIO the README uses `USE_SSL false` and `URL_STYLE 'path'`. For R2 use its HTTPS S3 endpoint and validate the required region/options in a disposable test. [2]

Default cache budgets are 512 MiB data and 128 MiB metadata, independent of DuckDB's buffer-manager limit. Set small PoC budgets before attach, e.g. 64 MiB data and 16 MiB metadata, and record actual RSS. Spill files remain local; main database and WAL/recovery files belong to the object-storage layer. [2]

Only one read-write process is supported. The extension does not implement up-front file locking: a second writer bumps SlateDB's writer epoch and takes ownership; the old writer fails on a later flush/checkpoint. External single-writer coordination is therefore necessary. Read-only independent processes may lag roughly ten seconds, while reads through the writer process see its committed state. [2]

A proper durability test kills the writer immediately after an acknowledged committed write, before graceful close or an intentional checkpoint, then reopens from a fresh process/container. A fencing test records when a displaced writer detects takeover and whether it can still acknowledge commits before that point. Continued acknowledgments are a safety finding requiring external single-writer coordination, not evidence of up-front locking. Neither test result is known yet.

## Existing project constraints

The Rust crate pins `duckdb = 1.20001.0` with native bundled DuckDB and Rust 1.100. Native FastEmbed/ONNX is an unconditional build dependency even when provider inference is disabled at runtime. The root service currently opens native filesystem DuckDB paths; it cannot directly use `duckdb_objfs://` without an adapter. Initial object-storage tests must be clearly labeled storage compatibility tests, not evidence that the unmodified service already persists to R2.

## References

[1]: https://developers.cloudflare.com/containers/get-started/ "Cloudflare Containers image architecture and Docker deployment prerequisites"
[2]: https://github.com/dentiny/duckdb-object-storage "Object-storage extension setup caches secrets and writer fencing"
[3]: https://duckdb.org/community_extensions/extensions/duckdb_object_storage "DuckDB community extension compatibility and installation"

## What to enable on Cloudflare

Verified against official documentation on 26 November 2026. Nothing on Cloudflare needs enabling for the two local Docker proofs. The following applies to preparing the later live deployment and optional real-R2 storage test.

1. **Workers Paid on the same account.** This is an account-level developer-platform plan with a minimum $5 USD monthly charge, not the zone-level Pro/Business plan. Containers have included usage plus additional metered compute/memory/disk and possible egress charges; $5 is not a flat total-cost guarantee. The observed Containers API error is the current entitlement blocker. [4]
2. **Check Workers & Pages > Containers.** After plan activation, check the Containers dashboard is accessible. The official deployment flow creates/configures the Worker, Container and Durable Object through Wrangler; the user need not manually create those resources now. Images must support Linux AMD64. [1]
3. **R2 subscription.** Open Storage & databases > R2 > Overview and complete the R2 checkout/subscription flow only if prompted. R2 includes free monthly usage but is billed for excess usage. The successful account bucket-list API check suggests R2 API access already exists, so do not assume activation is missing. [5]
4. **Private disposable bucket and S3 credentials, when the R2 test is ready.** Use a dedicated bucket such as `agentrecall-poc`, Standard storage, public access disabled. From R2 Overview > Account Details > API Tokens > Manage, create an account or user R2 API token with Object Read & Write, scoped only to that test bucket. Keep the resulting Access Key ID and Secret Access Key private; an ordinary dashboard/API bearer token is not interchangeable with these S3 credentials. Do not use an all-buckets Admin token or paste the secret into chat. Save the bucket's displayed S3 endpoint, accounting for its jurisdiction. [6]

A custom domain, DNS changes, public R2 access, D1, Vectorize and Cloudflare Tunnel are not prerequisites for this topology: the official Container example uses a `workers.dev` URL and defines its Durable Object in deployment configuration. [1] Billing permissions or a Global API Key need not be granted merely to complete the product prerequisites.

No product activation, subscription purchase, bucket creation, key issuance or deployment was performed while checking these instructions. It is reasonable to postpone Workers Paid until the local compatibility proofs justify cloud deployment.

[4]: https://developers.cloudflare.com/workers/platform/pricing/ "Workers Paid account plan Containers usage charges and separation from zone plans"
[5]: https://developers.cloudflare.com/r2/get-started/ "R2 account subscription setup and checkout"
[6]: https://developers.cloudflare.com/r2/api/tokens/ "R2 S3 access keys bucket-scoped Object Read and Write permissions and jurisdiction endpoints"

## Post-upgrade check — 26 November 2026, 09:59 CET

The user reported upgrading to Workers Paid. A fresh read-only GET to `/accounts/6c27fb90376881b1dd83114b986f17ab/containers/applications?name=agentrecall-poc&per_page=100` now returns `success: true`, no errors and an empty application list. The earlier paid-plan access error is cleared for this API read. This does not prove image-push/deployment permissions or a running service; no billable container was started.

A fresh default-jurisdiction R2 bucket listing filtered by `agentrecall` also returns `success: true` and no matching buckets. This check does not inspect other jurisdictions or establish S3 credential availability. No bucket, key, Worker, Container or other resource was created by the assistant. The plan-upgrade action was performed by the user, not through this task.

Both local proofs can still proceed independently of live R2. The current implementation plan remains pending review; no proof implementation or build/test has started.
