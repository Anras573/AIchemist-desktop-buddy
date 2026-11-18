# Settings window frontend

The settings window (spec sections 4.5 and 8). Its own Vite entry, so none of
its code is loaded in the buddy window.

- `index.html`, `settings.css`: static form markup and styles (no inline
  scripts or styles; CSP).
- `main.ts`: loads settings with `get_settings` and `list_monitors`, applies
  every change with `update_settings`, shows field errors.
- `form.ts`: types mirroring the Rust contract and pure parsing/mapping
  helpers.
- `accounts-view.ts`: the "Calendar accounts" section (its own `<form>`, so it
  is not part of the settings apply-on-change): account rows, add account,
  test connection, included calendars, sign in, change password, remove
  (confirmed inline, no dialogs). Refreshes on `accounts-changed`.
- `accounts.ts`: pure helpers for the accounts section (request builders,
  labels, error mapping), tested in `tests/frontend/accounts.test.mjs`.

Plain TypeScript, no framework: one static form does not need one.
