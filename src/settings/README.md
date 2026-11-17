# Settings window frontend

The settings window (spec sections 4.5 and 8). Its own Vite entry, so none of
its code is loaded in the buddy window.

- `index.html`, `settings.css`: static form markup and styles (no inline
  scripts or styles; CSP).
- `main.ts`: loads settings with `get_settings` and `list_monitors`, applies
  every change with `update_settings`, shows field errors.
- `form.ts`: types mirroring the Rust contract and pure parsing/mapping
  helpers.

Plain TypeScript, no framework: one static form does not need one.
