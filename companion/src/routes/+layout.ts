// The page options that make this a static SPA, set once for every route.
//
// `ssr = false`: there is no server. The Gateway is a Rust binary serving
// files, and the Companion does its cryptography in the browser (ADR 0014), so
// nothing would be gained by rendering markup in Node — and a component that
// happened to work under SSR would be a component that breaks the moment it
// touches `crypto.subtle`. Every route inherits this; a route that needs
// dynamic segments (`/networks/[network]`) is client-rendered like the rest and
// is answered by the fallback the Gateway serves.
//
// `prerender = true`: the routes that *are* statically known still get their
// own HTML file, so `/diagnostics` is `diagnostics.html` on disk and the
// Gateway serves it directly (step 2 of its resolution order) rather than
// falling back. Any route it cannot enumerate falls through to `200.html`,
// which the Gateway answers with HTTP 200.
//
// `trailingSlash = 'never'`: the chosen half of the contract with the Gateway.
// It makes the adapter write `diagnostics.html` rather than
// `diagnostics/index.html`, which is the spelling
// `companion-gateway/src/static_files.rs` resolves at step 2 — and its step 3
// redirects `/diagnostics/` onto `/diagnostics`, so the two agree in both
// directions. `'never'` is also SvelteKit's default: the Gateway handles both
// spellings, and this is the one that needs no configuration to stay true.

export const ssr = false;
export const prerender = true;
export const trailingSlash = 'never';
