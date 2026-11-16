# AIchemist Desktop Buddy

A small, always-running desktop companion: a pixel-art cat that reminds you of upcoming
meetings and your own tasks. Built with [Tauri 2](https://tauri.app/) (Rust core) and a
plain TypeScript/HTML/CSS frontend. See [`docs/spec.md`](docs/spec.md) for the full specification.

## Prerequisites

- [Rust](https://rustup.rs/) stable (1.90 or newer)
- [Node.js](https://nodejs.org/) 20.11 or newer (CI uses Node 24) and npm
- Platform dependencies for Tauri, see <https://tauri.app/start/prerequisites/>:
  - **Windows:** Microsoft C++ Build Tools (Visual Studio 2022 "Desktop development with C++") and WebView2 (preinstalled on Windows 10/11).
  - **macOS:** Xcode Command Line Tools (`xcode-select --install`).
  - **Linux (best-effort):** `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `libxdo-dev`, `libssl-dev`, `build-essential`.

## Run in development

```sh
npm install
npm run tauri dev
```

This starts the Vite dev server on port 1420 and opens the buddy window.

## Build and check

```sh
npm run build                 # typecheck + build the frontend into dist/
npm run tauri build           # release build and installers (unsigned)

cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                    # needs dist/ to exist, so run `npm run build` first
```

## Repository layout

```
src-tauri/          Rust core
src/buddy/          Buddy window frontend
src/settings/       Settings window frontend (later)
assets/sprites/     Default sprite pack (later)
tools/              Placeholder sprite generator (later)
docs/               Specification and docs
.github/workflows/  CI
```

## License

[MIT](LICENSE)
