# Capture Viewer

Full-screen 16:9 viewer for USB capture cards, written in Rust and compiled to WebAssembly
(`wasm-bindgen` + `web-sys`, built with [Trunk](https://trunkrs.dev)). Installable as a PWA.

**Shortcuts:** `F` fullscreen · `Space` pause · `Enter` start

## Develop

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk
trunk serve            # http://127.0.0.1:8080 (localhost counts as a secure context)
```

## Deploy to GitHub Pages

1. Push to a repo's `main` branch.
2. **Settings → Pages → Build and deployment → Source: GitHub Actions.**
3. The workflow in `.github/workflows/pages.yml` builds and publishes on every push.

## Layout

| Path | Purpose |
| --- | --- |
| `src/app.rs` | App state, capture lifecycle, UI wiring |
| `src/util.rs` | DOM / JS interop helpers |
| `index.html`, `style.css` | Shell and styling (Gruvbox + charcoal themes) |
| `public/` | Manifest, service worker, icons (copied to the site root) |
