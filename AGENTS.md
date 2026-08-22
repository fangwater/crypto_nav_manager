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

Re-check the SSH destination, working directory, service status, and remote
worktree before every deployment. The production worktree can contain operator
changes. Never use `git reset`, `git clean`, a forced checkout, or an
unreviewed pull to deploy, and never overwrite unrelated remote source files.

For frontend-only changes, upload the required source to a uniquely named
temporary directory on the remote host, install from the lockfile when needed,
run the production build there, and switch `frontend/dist` only after the build
succeeds. Preserve the previous dist as a rollback target until HTTP smoke
checks pass, then remove only the exact temporary and rollback paths created by
that deployment. Do not run Vite's development or preview server remotely.

Production Nginx serves `frontend/dist` at `/nav/` and proxies `/nav-api/` to
the Rust API. A frontend-only dist switch should not restart either service.
For Rust changes, build a release binary before replacing it, restart only
`crypto-nav-manager.service`, and verify both the unit state and `/api/health`.
Do not restart PostgreSQL, Nginx, trading processes, or unrelated services as
part of this deployment.

After deploying, verify `/nav/`, its emitted static assets, and
`/nav-api/health` through the existing Nginx gateway on port 4191. Verify any
affected API or deep link that the gateway supports. If verification fails,
restore the saved dist or binary atomically and report the failure.

## Worktree Safety

Assume uncommitted and untracked files belong to the user or an operator.
Inspect overlapping changes and work with them; leave unrelated files alone.
Never include credentials, private environment values, or remote `env.sh`
contents in repository files, logs, or chat output.
