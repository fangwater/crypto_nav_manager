# Repository Guidelines

## Build And Verification

Use focused checks for the files being changed. Frontend changes should pass
TypeScript compilation, Oxlint, the relevant verification scripts, and a Vite
production build. Rust changes should pass formatting and appropriately scoped
Cargo checks/tests before release builds.

Do not start local development, preview, or application servers as part of the
normal verification workflow. In particular, do not run local Vite listeners
or a local `crypto_nav_manager` service unless the user explicitly requests a
local runtime check. Non-listening builds, linters, tests, and direct checks
against an already-running service are allowed.

## Remote Deployment

After an approved code or UI change is verified, deploy it to the remote
environment instead of starting it locally, unless the user explicitly says
not to deploy.

The current production deployment is:

```text
SSH alias: jp-meta-elvpn
Directory: /home/ubuntu/crypto_nav_manager
Service: crypto-nav-manager.service (system service)
Frontend: /home/ubuntu/crypto_nav_manager/frontend/dist
Gateway: Nginx on 4191 (`/nav/` and `/nav-api/`)
```

All builds run on the local powerleader machine. The remote host only keeps
its git worktree synchronized with `origin/master` and receives the built
artifacts; do not run `cargo build`, `npm`, or Vite builds on the remote host.
Both machines are currently Ubuntu 24.04 x86_64 (glibc 2.39), so locally built
binaries are directly compatible — re-check `ldd --version` on both sides if
either OS changes.

Before every deployment, re-check the SSH destination, working directory,
service status, and remote worktree state. Commit and push local changes
first, then sync the remote worktree to `origin/master` (fetch + switch). If
the remote worktree ever contains uncommitted or unpushed operator changes,
preserve them before aligning (e.g. `git stash -u`, a `backup/` branch, and a
diff patch under `/tmp`) and never discard them silently.

For frontend changes, run the production build locally (`npm ci` from the
lockfile when needed, then `npm run build`), upload the resulting `dist` to a
uniquely named temporary directory on the remote host, and switch
`frontend/dist` only after the upload succeeds. Preserve the previous dist as
a rollback target until HTTP smoke checks pass, then remove only the exact
temporary and rollback paths created by that deployment. Do not run Vite's
development or preview server remotely.

Production Nginx serves `frontend/dist` at `/nav/` and proxies `/nav-api/` to
the Rust API. A frontend-only dist switch should not restart either service.
For Rust changes, run `cargo build --release` locally, upload the binary to a
uniquely named temporary path on the remote host, atomically swap it into
`target/release/crypto_nav_manager`, restart only
`crypto-nav-manager.service`, and verify both the unit state and
`/api/health`. Keep the previous binary as a rollback target until
verification passes. Do not restart PostgreSQL, Nginx, trading processes, or
unrelated services as part of this deployment.

After deploying, verify `/nav/`, its emitted static assets, and
`/nav-api/health` through the existing Nginx gateway on port 4191. Verify any
affected API or deep link that the gateway supports. If verification fails,
restore the saved dist or binary atomically and report the failure.

## Authentication And Permissions

All `/api/*` endpoints except `/api/health` and `/api/auth/login` require the
`nav_session` cookie (14-day DB-backed session in `nav_sessions`; in-memory on
read-only instances). Users live in `nav_users` (`admin` / `user`); regular
users only see strategies granted in `nav_user_strategy_grants` and all write
plus `/api/admin/*` endpoints are admin-only. There is no self-registration.

Create the first administrator directly in PostgreSQL:

```sql
INSERT INTO nav_users (username, password_hash, role)
VALUES ('admin', crypt('<password>', gen_salt('bf', 10)), 'admin');
```

Passwords use pgcrypto `crypt()` bcrypt hashes, so SQL inserts and the
`/api/admin/users` API produce identical rows. Admins manage users and
per-user strategy grants from `/nav/` "用户" page; `/api/auth/password` lets
any signed-in user rotate their own password (revoking other sessions).

## Worktree Safety

Assume uncommitted and untracked files belong to the user or an operator.
Inspect overlapping changes and work with them; leave unrelated files alone.
Never include credentials, private environment values, or remote `env.sh`
contents in repository files, logs, or chat output.
