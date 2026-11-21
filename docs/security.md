# Security

How AIchemist Desktop Buddy protects the user's accounts and calendar data, what the
security review (task 4.4) found, and how to report a vulnerability. Requirements come from
`docs/spec.md` section 9; the IPC surface is defined in `docs/contracts.md`.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting on the
repository (**Security > Report a vulnerability**,
<https://github.com/Anras573/AIchemist-desktop-buddy/security/advisories/new>). Include the
version, the OS, and steps to reproduce. Never include real passwords, tokens or calendar
data; fake values are enough. Fixes ship in a normal release, and the advisory is published
once a fixed version is available.

## Threat model

The app is a single-user desktop program. It runs with the user's rights, stores data in the
user's profile, and talks to the network only on the user's behalf.

**Assets**

- CalDAV passwords (often app-specific passwords) and Microsoft refresh tokens.
- Calendar contents (titles, times, locations) and task titles: personal data.
- The account sign-in name (email address) and CalDAV user name.

**In scope**

- A malicious or compromised network path (public Wi-Fi, TLS-inspecting proxy without its CA
  installed): mitigated by https only and OS certificate validation.
- A misbehaving or partly compromised calendar server, or an open redirect on it, trying to
  get the credentials sent to another host.
- Hostile calendar data (event titles, iCalendar or JSON payloads, huge recurrences) reaching
  the webviews or the parsers.
- Secrets leaking through logs, error messages shown in the UI or tray, or files on disk.
- A webview compromise (for example script injection through event data) trying to widen its
  reach through IPC.
- Other local processes or web pages talking to the OAuth loopback listener.
- Supply chain: vulnerable dependencies, mutable CI actions.

**Out of scope**

- Malware running as the same user, or an administrator. It can read the keychain entries
  the app can read, the cache and the log, and attach to the process.
- Physical access to an unlocked session.
- The security of the calendar servers, Microsoft Entra ID and GitHub themselves.

## What is protected, and how

### Secrets

- Passwords and refresh tokens live only in the OS keychain (Windows Credential Manager, the
  macOS login keychain) under the service `io.github.anras573.aichemistbuddy`, one entry per
  account and secret kind (`src-tauri/src/secrets`). Access tokens stay in memory.
- `SecretString` (`domain/secret.rs`) has no `Serialize` and a redacted `Debug`
  (`SecretString(***)`), so a secret cannot be returned over IPC or written to
  `settings.json`, and `{:?}` never prints it. Secrets travel only from the settings webview
  to the core.
- `settings.json` holds account ids, display names, server URLs, CalDAV user names, Graph
  client ids and tenants: configuration, no secrets. A server URL with embedded credentials
  is rejected. `account-identities.json` holds the Microsoft sign-in name per account
  (personal data, not a secret, never logged).

### IPC and webviews

- Two webviews: `buddy` (always present, including the popover) and `settings` (on demand).
  Each has one capability file (`src-tauri/capabilities/`) that grants exactly the commands
  `docs/contracts.md` assigns to it, plus `core:event:allow-listen`. The buddy also gets
  two window plumbing commands (`set_hit_regions`, `set_popover_open`), which also check the
  caller's window label. No `core:default`, no shell, fs, http, opener or dialog plugin
  permissions, no `remote` access.
- Every app command is declared in `build.rs` (`AppManifest::commands`), so it gets an
  `allow-*` permission and is denied to any window whose capability does not grant it.
- Tests in `src-tauri/src/lib.rs` pin this: the declared commands equal the registered
  handlers and the contract plus plumbing; each capability grants exactly its window's set;
  only the two capability files exist.
- Content Security Policy (production): `default-src 'self'; script-src 'self';
  style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self' ipc:
  http://ipc.localhost; object-src 'none'; base-uri 'self'; form-action 'none';
  frame-ancestors 'none'`. No `unsafe-inline` or `unsafe-eval`, no remote sources. The dev
  CSP only adds `style-src 'unsafe-inline'` and the Vite websocket for hot reload.
  `freezePrototype` is on, `withGlobalTauri` is off. The pages use the standard
  `window.__TAURI_INTERNALS__.invoke` path (what `@tauri-apps/api` wraps) and
  `plugin:event|listen`; they never unlisten or emit.
- Calendar and task text is put into the DOM with `textContent` only; there is no
  `innerHTML`, `eval` or `new Function` in the frontend.
- The reminder chime is triggered by an app-internal `play-chime` event to the buddy window.
  Before task 4.4 Rust ran a fixed script through `WebviewWindow::eval`; nothing in the app
  evaluates scripts in a webview any more.

### Network

The only outbound traffic:

| Destination | When | Protection |
|---|---|---|
| The configured CalDAV server | Sync, test connection, calendar list | `https` only (settings validation and ureq `https_only`). Redirects are followed by the app, not ureq, at most 5, only to `https`, and only to the configured server's site; the same check applies to server-sent hrefs and calendar ids, so Basic credentials never go to another site (see below). Bodies capped at 16 MiB. |
| `login.microsoftonline.com` | Sign-in, token refresh | Authorization code flow with PKCE (S256) and a random `state`, checked in constant time. Tenant and client id are validated before they go into a URL. |
| `graph.microsoft.com/v1.0` | Sync | Bearer token. `@odata.nextLink` is followed only if it starts with `https://graph.microsoft.com/v1.0/`, at most 200 pages. ureq strips the `Authorization` header on any redirect (its default) and refuses non-https URLs. Bodies capped at 4 MiB. |
| `api.github.com` (one fixed URL) | Opt-in update check, at most daily | Generic User-Agent, no identifiers. The tag is validated before it is used in a URL or the tray. |

TLS goes through the OS (SChannel, Security.framework) with the OS root store.

**CalDAV site rule.** A request is sent only to the configured host, its subdomains, or, when
the configured host has three or more labels, any host under its parent domain. iCloud needs
this: `caldav.icloud.com` redirects and points discovery to partition hosts such as
`p42-caldav.icloud.com`. There is no public-suffix list, so a server configured as
`example.co.uk` would also accept `*.co.uk`; the redirect would still have to come from the
configured server over https.

**Browser launches.** The app opens the default browser only for
`https://login.microsoftonline.com/...` (sign-in) and
`https://github.com/Anras573/AIchemist-desktop-buddy/releases/...` (update available), through
the OS launcher with the URL as a single argument and no shell.

**OAuth loopback listener.** Bound to `127.0.0.1` on an OS-chosen port only during an
interactive sign-in, with a timeout and cancel. It answers only `GET /`; any other path gets a
404, other methods 405, oversized heads 431. The first request carrying `code` or `error` ends
the wait. Responses are fixed pages that never echo request data, with
`Cache-Control: no-store`. Only the sanitised OAuth error code is kept, never
`error_description` (it can contain the user's name).

### Logging

- One file, `aichemist-buddy.log` in the OS app log directory, rotated at 1 MiB to
  `aichemist-buddy.old.log` (at most 2 MiB kept). `info` and up for the app, `warn` and up for
  dependencies. Debug builds also echo to stderr.
- Messages contain fixed text, error kinds, status codes, counts, random account ids and
  release tags. They never contain passwords, tokens, auth codes, PKCE verifiers, URLs with
  credentials, event or task contents, or email addresses.
- Error messages shown in Settings and the tray follow the same rule (`docs/contracts.md`,
  Errors). Provider errors are fixed strings plus HTTP status and sanitised OAuth or Graph
  error codes; JSON parse errors report only the error class and position.
- The frontend logs only failed IPC calls (`console.error`), which reach nobody but the
  webview's dev tools.

### Files on disk

| File | Location | Contents |
|---|---|---|
| `settings.json` | OS app config dir | Configuration, no secrets |
| `account-identities.json` | OS app config dir | Sign-in name per Graph account |
| SQLite database | OS app data dir | Cached events (next 7 days), tasks, reminder ledger, sync state |
| `aichemist-buddy.log`, `.old.log` | OS app log dir | See Logging |

All are in the user's profile and created with the platform defaults (per-user ACLs on
Windows, the user's umask under `~/Library` on macOS). Cached events are personal data kept
locally so reminders fire offline; removing an account deletes its cached events.

### Supply chain and CI

- `ci.yml` runs with `permissions: contents: read` and uses no secrets (not even
  `GITHUB_TOKEN` beyond checkout). Third-party actions are pinned to commit SHAs.
- A `dependency audit` job runs `cargo audit` and `npm audit --omit=dev` on every push and
  pull request.
- No telemetry, analytics or crash reporting. Dependencies are kept few and justified in
  `Cargo.toml`.

## Review findings (task 4.4, 2026-11-21)

| # | Finding | Severity | Status |
|---|---|---|---|
| 1 | CalDAV followed `https` redirects, and server-sent hrefs, to **any** host with Basic credentials. An open redirect on the configured server could send the password elsewhere. | Medium | **Fixed.** Requests go only to the configured server's site (rule above); tests cover iCloud partition hosts and a refused cross-site redirect. |
| 2 | The sign-in browser opener checked `starts_with("https://login.microsoftonline.com")` without a trailing `/`, so `https://login.microsoftonline.com.evil.example/` would pass. Not reachable today (the URL is built from a validated tenant), but a weak guard. | Low | **Fixed.** Prefix now includes `/`; test added. |
| 3 | A malformed token response produced `malformed token response: {serde_json error}`; serde_json can quote the offending value, and the message reaches the log and the Settings UI. | Low | **Fixed.** Only error class and position, as the Graph provider already did; test added. |
| 4 | The chime was triggered with `WebviewWindow::eval` of a fixed string and a global `window.__aichemistPlayChime`. The string was a `const`, so no data could reach it, but `eval` is an easy place for a future change to inject data, and any page script could call the global. | Low | **Fixed.** App-internal `play-chime` event to the buddy window; no `eval`, no global. `domain/ipc.rs` unchanged. |
| 5 | CSP allowed `asset:` in `img-src`, but the asset protocol is not enabled or used. | Info | **Fixed.** Removed from both CSPs. |
| 6 | Grants were checked only by spot tests. | Info | **Fixed.** Tests now pin manifest = handlers = contract + plumbing, exact grants per window, no extra capability files, and the CSP and webview flags. |
| 7 | CI actions were referenced by mutable tags (`@v5`, `@v2`, `@stable`). | Low | **Fixed** in `ci.yml` (pinned SHAs). `perf.yml` is owned by task 4.2 and still uses tags: **deferred** to that task. |
| 8 | No automated dependency audit. | Low | **Fixed.** `dependency audit` CI job (prebuilt `cargo-audit` via a pinned `taiki-e/install-action`, plus `npm audit --omit=dev`). |
| 9 | The buddy capability grants six contract commands its page does not call today: `list_tasks`, `update_task`, `snooze_all`, `open_settings`, `get_paused`, `set_paused` (the cat menu and tray do these in Rust). | Low | **Deferred (owner decision).** The contract requires capabilities to grant exactly its list. All six act only on the user's own tasks and reminder state. Removing them needs a contract change. |
| 10 | `cargo audit`: no vulnerabilities. Warnings: `chrono` 0.4 unmaintained (RUSTSEC-2026-0543, direct dependency, no fix); `glib` 0.18 unsound (RUSTSEC-2024-0429), `zbus_names`, `zvariant`, `zvariant_derive`, `proc-macro-error` unmaintained. All but `chrono` come only from the Linux GTK stack, which is best-effort. | Info | **Accepted.** Revisit when Tauri moves to newer GTK bindings or a `chrono` successor is settled. `npm audit --omit=dev`: 0 (no runtime npm dependencies); full `npm audit`: 0. |
| 11 | ureq follows up to 10 redirects for Graph and the update check. | Info | **Accepted.** ureq drops `Authorization` on every redirect by default and refuses non-https; the update check must survive a renamed repository. |
| 12 | The OAuth loopback listener ends the wait on the first request with `code` or `error`; a local process could send a fake `error` to abort a sign-in. | Info | **Accepted.** Denial of service only, by a local process that is out of scope; PKCE and `state` stop code injection. |
| 13 | The keychain service name is fixed to the release identifier, so a dev build with another identifier shares the entries. | Info | **Accepted.** Same user, same data; documented here. |
| 14 | WebView2 runs with `msSmartScreenProtection` disabled (memory tuning, `docs/perf.md`). | Info | **Accepted.** The webviews load only bundled local pages and never navigate to remote content. |
| 15 | `macOSPrivateApi` is on (transparent buddy window). | Info | **Accepted.** No security effect; only rules out the Mac App Store. |
| 16 | Files are created with platform default permissions. | Info | **Accepted.** They live in per-user directories; secrets are not in them. |
| 17 | The single-instance plugin listens on a per-user Unix socket on macOS (named mutex and message window on Windows). | Info | **Accepted.** A message only makes the running app show an "already running" notice. |

## Checklist for future changes

- A new command: add it to `build.rs`, the handler list, `domain::ipc::COMMANDS` (through the
  orchestrator) and the one capability that needs it. The tests in `lib.rs` fail otherwise.
- A new outbound request: add it to the network table above. Keep `https` only and never
  forward credentials to a host the user did not configure.
- A new log line or error message: no secrets, no event or task contents, no email addresses
  at `info` or above.
- Webview content: `textContent`, never `innerHTML`; no remote scripts, styles or images.
