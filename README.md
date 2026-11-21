# qgate

## Overview

qgate is a local quality gate for your git repositories. It runs on your machine (macOS, Windows or Linux with
Docker) as **two containers**: `qgate` (holds the secrets, talks to Claude/Jira/Figma, serves the dashboard) and
`qgate-runner` (runs untrusted code: model-written tests, repository checks, build scripts; no secrets, see
"Security notes"). It reviews on **`git push`**, not on every local commit: when a watched repository pushes a
branch, qgate reviews the whole branch (`merge-base(<base-branch>, head)..head`) and:

- reviews the change with Claude (bugs, security, maintainability, your repo's own guidelines),
- checks the Jira ticket's acceptance criteria when a ticket key is in the branch name or commit message,
- runs deterministic checks you configure (tests, linters) with the toolchains you chose,
- runs a deterministic quality gate (dependency CVEs, secret leaks, architecture boundaries) and a regression
  check (existing tests of impacted Go packages, base vs head),
- shows the results on a local dashboard and, optionally, posts them to Slack.

Your repositories are mounted **read-only**: qgate never changes your code.

## When a review runs

qgate watches `refs/remotes/<remote>/*` of every repository. A review is triggered when the reflog of a remote-tracking
ref shows `update by push` or `forced-update`; `fetch` and `pull` are ignored. The reviewed range is
`merge-base(<base-branch>, head)..head`, so the whole branch is judged, not only the last commit.

- **Base branch:** admin setting `watch.base_branch` (env `QGATE_WATCH_BASE_BRANCH`; default: `origin/main`, else
  `origin/master`). A repository can override it in `.qgate.toml`. Pushes to the base branch itself (main/master)
  are not reviewed.
- **`[WIP]`:** if the last commit subject contains `[WIP]`, acceptance criteria show progress only and never block
  the gate.
- **Cache:** results are content-addressed (keyed by git tree/blob hashes plus model, prompt and toolchain), so the
  2nd and later pushes of a branch reuse everything that did not change. A `qgate: no-cache` commit trailer
  bypasses the cache.

## What is checked beyond the model review

Two deterministic checks cover what SonarQube does not. They run in the runner container and fold into the review
findings and the gate; findings carry prevention rules into `qgate-rules.md` (see "Review files").

- **Quality gate (Go/Java), graded on NEW code only:** dependency CVEs (`govulncheck`, `osv-scanner`), secret leaks
  (`gitleaks`) and architecture import boundaries (`depguard`, generated from approved Rulebook rules). It
  deliberately does **not** duplicate SonarQube: no coverage, complexity, duplication, code smells or generic SAST.
- **Regression:** finds the Go packages impacted by the diff and re-runs their existing tests on base and on head. A
  test that passed on base and fails on head (reconfirmed) is a regression.

## Quickstart (Docker)

Requires Docker Desktop (macOS/Windows) or Docker Engine with the Compose plugin (Linux).

1. **Configure:** `cp qgate.env.example qgate.env`, then set `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token` on a machine logged in to your Claude subscription; see "Claude authentication" below) and `REPOS_PATH` (the folder
   that contains your git repositories, e.g. `/Users/me/code` or `C:/Users/me/code`).
2. **Start:** `docker compose up -d`
   (the first start builds the image and installs Go + Java into a cache volume: a few minutes).
3. **Log in once:** open the link from `docker compose logs qgate | grep Dashboard`
   (`Dashboard: http://localhost:8080/login?token=...`, or run `docker compose exec qgate qgate login-url`).
   The browser then stays logged in for 30 days; afterwards the dashboard is at <http://localhost:8080>.
4. **Push** a branch from any repository under `REPOS_PATH` (not main/master): the review appears on the dashboard
   shortly after the push. Local commits alone do not trigger a review.

Useful commands:

| What | Command |
|---|---|
| Follow the logs | `docker compose logs -f qgate` (runner: `qgate-runner`) |
| Dashboard login link | `docker compose exec qgate qgate login-url` |
| Health check | `docker compose exec qgate qgate doctor` |
| Installed toolchains | `docker compose exec qgate qgate-toolchains status` |
| Apply changes to `qgate.env` | `docker compose up -d` |
| Update qgate | `git pull && docker compose up -d --build` |
| Stop | `docker compose down` |
| Uninstall (deletes history and toolchain cache) | `docker compose down --volumes --rmi all` |

**Change toolchains:** edit `QGATE_TOOLCHAINS` / the version variables in `qgate.env`, then run
`docker compose up -d` again. Only what is missing is downloaded; an older version stays installed next to the
new one, so switching back is instant.

## Configuration

qgate has three layers of configuration:

| File | Scope | Contents |
|---|---|---|
| `qgate.env` (from `qgate.env.example`) | Docker setup | Claude subscription token, repo folder, port, toolchains, Jira credentials, webhook, base branch, runner limits/network, container uid/gid |
| Settings page (<http://localhost:8080/settings>), stored in the `qgate-data` volume | Server | dashboard, repo scanning depth, poll interval, concurrency, base branch, Claude CLI binary/timeout, Jira connection and defaults, agents, Figma, review files. Seeded once from `config/qgate.toml` |
| `.qgate.toml` in each repository | Repository | model, effort, gate policy, base branch, ignored paths, checks (`[[checks]]`), Jira `enabled`/`ac_field`/`key_pattern_projects` |

**Server settings live in qgate's database** (`/data/qgate.db`, tables `server_settings` and
`settings_schema_version`). On the very first start, `config/qgate.toml` (created from `config/qgate.toml.example`)
is imported once as version 1; after that the file is **no longer read**. Change settings on the Settings page:
every save is kept as a new version (who/when/which fields), and the History section can roll back to any of the
last 20 versions (a rollback is saved as a new version too).

Environment variables win over the stored settings: `QGATE_BIND`, `QGATE_REPOS_ROOT`, `QGATE_WEBHOOK_URL`,
`QGATE_CLAUDE_BIN`, `QGATE_CLI_TIMEOUT_SECS`, `JIRA_BASE_URL`, `JIRA_EMAIL`, `QGATE_JIRA_ENABLED`, `QGATE_OUTPUT_DIR`,
`QGATE_JIRA_MCP_COMMAND` (space-separated), `QGATE_JIRA_MCP_ENV_PASSTHROUGH` (comma list), `QGATE_FIGMA_API_BASE_URL`,
`QGATE_FIGMA_MCP_URL`, `QGATE_RUN_REPO_CHECKS`, `QGATE_WATCH_BASE_BRANCH`. The Settings page shows those fields
locked ("set by QGATE_…"); env values are never written to the database.

**Admin-only settings** are never editable over HTTP (Settings page or Policy assistant), because any process
that can reach the dashboard could otherwise run another program with your Claude credentials or send secrets elsewhere: `llm.claude_bin`, `llm.timeout_secs`,
`jira.base_url`, `jira.mcp.command`, `jira.mcp.env_passthrough`, `figma.api_base_url`, `figma.mcp_url`, setting
`notify.webhook_url`, `watch.base_branch` and turning `watch.run_repo_checks` on. Set them with the environment variables above in
`qgate.env` (or in `config/qgate.toml` before the first start) and restart qgate. Clearing the webhook and turning
repository checks off stay possible on the Settings page. Every change made through the dashboard also needs a
per-start API token that the pages carry; after a restart, reload open pages. Most settings apply immediately; the
listen address, allowed hosts, repository folders, scan depth, poll interval, review concurrency and the
review-file folder need
`docker compose restart qgate` (the page tells you after saving).

Secrets (Jira API token, Figma token; the Claude token is only read from the environment) are **not** stored in the settings database: they come
from the environment variable, its `*_FILE` variant, or the credential store (0600 file in the config dir, or the
OS keychain natively). The Settings page can set or remove a stored secret (write-only, never shown again) unless
it comes from the environment. The webhook URL contains a token, so the page only shows whether one is configured.

Inside the `qgate` container: repositories are at `/repos`, data (SQLite history, logs, caches) in the `qgate-data`
volume at `/data`, the server config at `/config`, toolchains in the `qgate-toolchains` volume at `/toolchains`,
review snapshots in the `qgate-work` volume at `/work`. The `qgate-runner` container sees only `/work`, `/toolchains`
(read-only) and its own `qgate-runner-cache` volume at `/cache`; `/data`, `/config`, `/reviews` and `/repos` are
empty and unreadable there. Its network is `QGATE_RUNNER_NETWORK` (default `bridge`; `none` = fully offline).

Note on `qgate.env`: `docker-compose.yml` includes the service with `env_file: qgate.env`, so the same file
both configures the container and provides `REPOS_PATH`, `QGATE_DASHBOARD_PORT` and `QGATE_UID`/`QGATE_GID` to
Compose. `docker compose` commands therefore fail with `qgate.env: no such file` until you create it.

## Toolchains

The image contains no language toolchains, which keeps it small. On every start the entrypoint reads
`QGATE_TOOLCHAINS` (default `go,java,quality`; allowed `go`, `java`, `node`, `python`, `rust`, `quality`) and installs whatever is
missing into the `qgate-toolchains` volume. Second and later starts take well under a second.

| Toolchain | Version variable (default) | Source | Verification |
|---|---|---|---|
| go | `QGATE_GO_VERSION` (`latest`; or `1.24`, `1.24.3`) | go.dev | SHA-256 from `go.dev/dl/?mode=json` |
| java | `QGATE_JAVA_VERSION` (`21`, Temurin major) | Adoptium API v3 | `package.checksum` (SHA-256) |
| java: Maven | `QGATE_MAVEN_VERSION` (`3.9.11`; `none` to skip) | Apache downloads / archive | `.sha512` |
| java: Gradle | `QGATE_GRADLE_VERSION` (empty = skip) | services.gradle.org | `.sha256` |
| node | `QGATE_NODE_VERSION` (`24`; or `24.11.1`) | nodejs.org | `SHASUMS256.txt` |
| python | `QGATE_PYTHON_VERSION` (`3.13`), `QGATE_UV_VERSION` (`latest`) | uv (GitHub releases), then `uv python install` | uv: `.sha256`; CPython: hashes built into uv |
| rust | `QGATE_RUST_VERSION` (`stable`) | rustup-init (static.rust-lang.org) | `.sha256`; components verified by rustup |
| quality: gitleaks | `QGATE_GITLEAKS_VERSION` (`8.30.1`; `none` to skip) | GitHub releases | release checksums file (SHA-256) |
| quality: osv-scanner | `QGATE_OSV_SCANNER_VERSION` (`2.7.1`; `none` to skip) | GitHub releases | `osv-scanner_SHA256SUMS` |
| go: govulncheck | `QGATE_GOVULNCHECK_VERSION` (`v1.10.0`; `none` to skip) | `go install` (needs go) | Go module checksum database |
| go: golangci-lint | `QGATE_GOLANGCI_LINT_VERSION` (`2.15.0`; `none` to skip) | GitHub releases | release checksums file (SHA-256) |

How it works:

- One installer per toolchain in `docker/toolchains/` (bash, `set -euo pipefail`, amd64 and arm64).
- Downloads are HTTPS-only (also on redirects). A checksum mismatch or network error aborts **that toolchain
  only**: it is logged, the others continue and qgate still starts (checks needing it will fail).
- Each requested version lives in `/toolchains/<tool>/<version>`; `/toolchains/<tool>/current` is a symlink
  switched atomically. Moving versions like `latest`, `21` or `24` are resolved once, at first install; to
  upgrade, set a newer explicit version.
- `flock` on `/toolchains/.lock` stops two containers sharing the volume from installing at the same time.
- Build caches (Go modules, `~/.m2`, Gradle, npm, uv, pip) live in `/data`, so they persist across restarts.
- `docker compose exec qgate qgate-toolchains status` lists what is installed; `... install` re-runs the installer.

**C compiler:** the image has no `gcc`. Go builds fine without it (cgo is disabled automatically), but Rust crates
and native Node/Python extensions need one. Add it at build time with the override file:
`QGATE_APT_PACKAGES: build-essential` (see below), then `docker compose up -d --build`.

**Pre-installed toolchains (CI / offline):** copy `docker-compose.override.example.yml` to
`docker-compose.override.yml` and set the build arg `QGATE_PREINSTALL: go,java`. The same scripts run during
`docker build` and the build fails if any download or checksum fails. A new `qgate-toolchains` volume is
seeded from the image; an existing volume keeps its content (remove it with
`docker volume rm qgate_qgate-toolchains` after changing what you bake). Without Compose:

```sh
docker build --build-arg QGATE_PREINSTALL=go,java,node -t qgate:full .
```

## Jira

1. Create an API token at <https://id.atlassian.com/manage-profile/security/api-tokens>.
2. In `qgate.env` set `QGATE_JIRA_ENABLED=true`, `JIRA_BASE_URL` (e.g. `https://yourcompany.atlassian.net`),
   `JIRA_EMAIL` and `JIRA_API_TOKEN`.
3. Recommended: on the Settings page (Jira section) add the allowed project keys (e.g. `PROJ`) so branch names
   like `release-2` are not mistaken for tickets; optionally the acceptance-criteria field (e.g. `customfield_10050`).
4. `docker compose up -d`.

qgate finds the ticket key (e.g. `PROJ-123`) in the branch name or commit message and checks the change against
the ticket's acceptance criteria. A repository can override `enabled`, `ac_field` and `key_pattern_projects` in
the `[jira]` section of its `.qgate.toml`; the connection (`base_url`, `email`, `provider`, `[jira.mcp]`) is
server-only (see "Security notes"). To use an MCP server instead of the REST API, choose provider "MCP" on the
Settings page; the default command (`uvx mcp-atlassian`) needs the `python` toolchain.

## Figma

qgate can compare a change with the Figma frames linked from the ticket. Links are read from the ticket's
**parent Story** (and the ticket itself), so designers only need to link the frame once on the Story.

1. Create a personal access token in Figma (Settings → Security → Personal access tokens, read-only file content).
2. Provide it as `FIGMA_TOKEN` in `qgate.env`, as a Docker secret with `FIGMA_TOKEN_FILE=/run/secrets/figma_token`,
   or paste it on the Settings page (stored in the credential store, never shown again).
3. On the Settings page, Figma section: enable it and pick the checks: **copy** (text in the frame missing from the
   code and its i18n files), **component** (the design uses a design-system component, the code renders something
   else), **token** (hard-coded colour/spacing/font where the design binds a variable), **states** (loading, empty,
   error, disabled variants the code does not handle). Copy, component and token can block the gate; states is
   judged by the model from code alone and never blocks.

Why the REST API and not Figma's MCP server: the cloud MCP server at `mcp.figma.com` only accepts OAuth from
clients listed in Figma's catalog, which a background service cannot complete unattended. qgate therefore uses the
Figma REST API with a personal access token by default; provider "MCP" is only for your own MCP bridge.

## Review files for your coding agent

After every review qgate writes a **review file** (`latest.md`) and a companion **`qgate-rules.md`** (prevention
rules derived from the findings) that you can hand to your coding agent (Claude Code, Cursor, …) to continue fixing
and developing the feature. Optionally it also writes a self-contained **HTML report** (`latest.html` /
`<sha7>.html`) to attach to Jira as evidence or print to PDF. The Markdown review is written for an LLM agent and a
human alike:

- the verdict (PASSED/FAILED) and why the gate failed;
- **Do this first (blocking)**: numbered, self-contained tasks: what is wrong, `path:line`, the evidence, the
  expected behaviour and a suggested fix. Failing generated tests include the acceptance criterion, Given/When/Then,
  the test file path and full source, the exact reproduce command and the failing output; failed checks include
  their output; approved architecture-rule violations and blocking Figma mismatches (design value vs code);
- **Should fix (warnings)**: the same shape for non-blocking items (major/minor findings, inferred rules,
  design states, inconclusive tests with the reason);
- the acceptance-criteria status table, ticket context (implicit requirements are marked "suggested, not
  required", Figma links), and short instructions for coding agents (fix blocking items in order, never weaken
  or delete the generated tests, re-run the reproduce commands, commit with the ticket key).

**Where:** set `REVIEWS_PATH` in `qgate.env` (default `./reviews`); it is mounted read-write at `/reviews`, the only
folder qgate writes to on your machine (on Linux create it first with `mkdir -p` so it is owned by you). Files are
named by the **path template** on the Settings page ("Review files"), default `{repo}/{branch}/{sha7}.md`.
Placeholders: `{repo}` `{branch}` `{sha7}` `{sha}` `{date}` (UTC `YYYYMMDD-HHMMSS`) `{ticket}` (`no-ticket` when
none) `{status}` (`passed`/`failed`). Values are sanitised to `[A-Za-z0-9._-]` (max 80 characters), so
`feature/ABC-1` becomes `feature-ABC-1`. The template must be relative, end with `.md` and contain no `..`; the HTML report uses the same name with `.html`.

**Always up to date:** `<repo>/<branch>/latest.md` (and `latest.html` when enabled) is rewritten after every review (also for passing runs, so it
never shows a stale failure). Point your agent at it, e.g. in the repository's `CLAUDE.md`:

```text
Before continuing work, read ~/qgate-reviews/shop-api/<branch>/latest.md and fix blocking items first.
```

Other settings (all apply without a restart, except the folder): enable/disable, **formats** (Markdown by default;
HTML report and JSON `qgate.review/v1` with the full report are opt-in: enable them in Settings → Review files), also write files for passing runs, and **keep last N**
(default 20 per folder): older *generated* review files are deleted, nothing else in the folder is touched. Pruning
only applies when the file name contains `{sha7}`, `{sha}` or `{date}`; otherwise each scan overwrites the same file.
Files are written atomically (0644), never through symlinks and never outside the folder. Writing a review file can
never fail the review itself: errors are logged.

**Native `qgate review`** (outside Docker) writes to `<repo>/.qgate/reviews/`: add `.qgate/reviews/` to the
repository's `.gitignore`.

## Dashboard

The dashboard is served at <http://localhost:8080> (change the port with `QGATE_DASHBOARD_PORT`). It shows the
reviews of every watched repository: verdict, findings, acceptance-criteria status and check results, plus
history and recurring issues. The container healthcheck uses `/healthz` (answers `ok`, no data).

**Login.** Every page and API needs a session cookie (Jupyter-style). qgate prints a login link at startup
(`Dashboard: http://localhost:<port>/login?token=<token>` in `docker compose logs qgate` or the output of
`qgate serve`); `qgate login-url` prints it again (`docker compose exec qgate qgate login-url`). Opening the link
sets an `HttpOnly`, `SameSite=Strict` cookie valid for 30 days and redirects to the dashboard; the login page
also accepts the pasted token. The token is generated on first start and kept in `<data dir>/dashboard-token`
(mode 0600, in the `qgate-data` volume, so it survives restarts), unless `QGATE_DASHBOARD_TOKEN` is set in
`qgate.env` (at least 32 URL-safe characters, e.g. `openssl rand -hex 32`). To log every browser out, delete that
file (or change `QGATE_DASHBOARD_TOKEN`) and restart. Treat the link like a password.

The port is published on `127.0.0.1` only, so the dashboard is not reachable from your network. The server also
rejects requests whose `Host` header is not in `dashboard.allowed_hosts` (DNS-rebinding protection).

The **Settings** page (<http://localhost:8080/settings>, linked from the dashboard header) edits the server
settings. Its write endpoints (`PUT /api/settings`, `POST /api/settings/rollback/{version}`,
`PUT`/`DELETE /api/secrets/{name}`) require `Content-Type: application/json` and the header
`X-Qgate-Request: settings`, reject an `Origin` that is not an allowed host and limit bodies to 64 KB, so another
website open in your browser cannot change them. They also need the dashboard session and a per-start API token
that only logged-in pages carry, so a process that can reach the port (e.g. test code in the runner container via
`host.docker.internal`) gets neither data nor write access without the login token.

## Claude authentication and usage limits

qgate does not use the Anthropic API (no API key, no per-token bill). Every agent runs the Claude Code CLI
(`claude -p`) installed in the `qgate` image (pinned release, SHA-256 verified; `QGATE_CLAUDE_VERSION` build arg) on
your Claude subscription (Max/Pro). The `qgate-runner` image is built from a separate Docker target and contains
neither `claude` nor any token. Authenticate the `qgate` container one of these ways:

1. **`CLAUDE_CODE_OAUTH_TOKEN` (preferred for automation).** On a machine where you are logged in run
   `claude setup-token` and put the token in `qgate.env` (or a Docker secret via `CLAUDE_CODE_OAUTH_TOKEN_FILE`).
2. **Log in inside the container:** `docker compose exec qgate claude login`; the login is stored in the
   `qgate-data` volume and survives restarts.
3. **Mount the host's `~/.claude` read-only** (example in `docker-compose.override.example.yml`). This hands your
   own Claude Code credentials, settings and history to the container, and on macOS the login lives in the
   Keychain (not in that folder), so prefer option 1 or 2.

`qgate doctor` (inside the container) checks that the CLI is installed. `llm.claude_bin` / `QGATE_CLAUDE_BIN` and
`llm.timeout_secs` / `QGATE_CLI_TIMEOUT_SECS` (admin-only) choose the binary and the time limit of one call.

**Usage limits.** Reviews draw on your subscription's usage. At the start of every review qgate sends one tiny
probe; if the limit is reached the review is **not run at all** and ends as an *error* (never a pass or a fail):
"qgate: not running - Claude usage limit reached, waiting for session reset (resets ...)". The same applies if a
limit is hit mid-review (the review aborts rather than passing with skipped work), and if the CLI is missing or
not logged in. Re-run the review after the reset. The remaining quota cannot be read, so qgate only logs a
best-effort estimate ("estimated ~N input tokens for this review") in `docker compose logs qgate`.

## Security notes

- **Secrets:** `qgate.env` contains your Claude subscription token and other credentials. Never commit it (add
  `qgate.env` to `.gitignore`), and make it private: `chmod 600 qgate.env`. Better still, use a Docker secret: put the
  token in `secrets/claude_oauth_token` (`chmod 600`), enable the `secrets:` block in
  `docker-compose.override.example.yml`, clear `CLAUDE_CODE_OAUTH_TOKEN` and set
  `CLAUDE_CODE_OAUTH_TOKEN_FILE=/run/secrets/claude_oauth_token`. `JIRA_API_TOKEN_FILE` and `FIGMA_TOKEN_FILE`
  work the same way. The Claude token (like everything in `qgate.env`) is never passed to `qgate-runner`.
- **Code leaves your machine:** diffs and relevant file contents are sent to Claude (through the Claude Code CLI on your subscription) for review (and
  ticket keys to your Jira site). Exclude sensitive paths with `review.ignore` in `.qgate.toml`.
- **Trust boundary:** a repository's `.qgate.toml` is controlled by anyone who can push a commit.
  - It may choose the model, effort, gate policy, ignored paths, Jira `ac_field`/`key_pattern_projects` and checks.
  - It can never choose endpoints or commands that receive credentials: `[llm] claude_bin`, `[jira] base_url`,
    `email`, `provider` and `[jira.mcp]` are server settings only (Settings page); a `.qgate.toml` setting them
    fails to load. HTTP redirects from the Jira API are never followed.
  - `[[checks]]` and the generated tests execute repository code (tests, linters, build scripts). In Docker they
    run in the separate `qgate-runner` container: no secrets, no access to qgate's database, config, review
    files or your repositories (only a per-commit snapshot; each test scenario gets its own), read-only
    toolchains, and no login token for the dashboard (it can reach the published port via `host.docker.internal`
    on Docker Desktop/colima, but every route needs the login cookie; `QGATE_RUNNER_NETWORK=none` makes it fully
    offline).
    Natively (`qgate review` without `QGATE_RUNNER_SOCKET`) they run as your user. Only watch repositories you
    trust, or turn off "Run the [[checks]] commands" in the Watcher section of the Settings page.
- **Container hardening (docker-compose.yml):** non-root user, read-only root filesystem (`/tmp` is a tmpfs),
  all capabilities dropped, `no-new-privileges`, repositories mounted read-only, dashboard bound to localhost.
  The only writable host folder is `REVIEWS_PATH` (`/reviews`, review files).
- **Supply chain:** toolchains are downloaded over HTTPS only and verified against the vendor's published
  SHA-256/SHA-512 before use. The checksums come from the same vendor (over TLS), so this protects against
  corrupted and tampered-in-transit downloads, not against a compromised vendor. Pin explicit versions and bake
  them into the image (`QGATE_PREINSTALL`) if you need reproducible builds.
- **Linux file permissions:** the container runs as uid/gid `QGATE_UID`/`QGATE_GID` (default 1000). If your
  repositories are not world-readable, set them to `id -u` / `id -g` and rebuild (`docker compose up -d --build`).
  If you change them after the first start, fix the volumes once:
  `docker compose run --rm --user root qgate chown -R "$(id -u):$(id -g)" /data /toolchains`.

## Development

Build and test the crate (Rust, edition 2024):

```sh
cargo build
cargo test
cargo run -- serve        # watcher + dashboard on 127.0.0.1:8080 (prints the login link)
cargo run -- login-url    # print the dashboard login link again
cargo run -- doctor       # health check
```

Container development:

```sh
docker build -t qgate:local .                       # image (toolchains not included)
docker compose up -d --build                        # rebuild and restart
docker compose run --rm qgate qgate-toolchains status
```

Layout: `src/<context>/{domain,application,infrastructure,api}` (bounded contexts: `watch`, `review`, `tickets`, `handoff`,
`history`, `settings`, `cli`); container files in `Dockerfile`, `docker-compose.yml`, `docker/` (service
definition, entrypoint, toolchain installers) and `config/qgate.toml.example` (first-start seed of the settings).
