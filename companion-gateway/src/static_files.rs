//! Resolving a request path to a file of the Companion's build.
//!
//! The Companion is a SvelteKit static export, so the Gateway has to resolve
//! paths the way the adapter's own preview server does — a prerendered page
//! is a file (`onboarding/whatsapp.html`, or `onboarding/whatsapp/index.html`
//! when the build keeps trailing slashes), and a route that was not
//! prerendered is client-side only and must be answered with the SPA
//! fallback. A dumb "file, or else the fallback" rule serves the wrong thing
//! for both.
//!
//! The order, from the adapter's preview server:
//!
//! 1. the exact file at the path;
//! 2. otherwise the path plus `index.html` when it ends in `/`, else the path
//!    plus `.html`;
//! 3. otherwise, when the path ended in `/` and the slash-less `.html`
//!    exists, a 307 to the slash-less path;
//! 4. otherwise, when the path plus `/index.html` exists, a 307 to the path
//!    with a trailing slash;
//! 5. on a match, 200 with the content type of the path's extension
//!    (`text/html` when it has none);
//! 6. otherwise the fallback file, with 200 — never a redirect, never a 404,
//!    so a deep link reloaded cold loads the app.
//!
//! The fallback is `200.html` by default, not `index.html`: the adapter's
//! documentation warns that an `index.html` fallback collides with a
//! prerendered homepage.

use std::path::{Path, PathBuf};

/// What a request path resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Serve this file, with this content type.
    File {
        path: PathBuf,
        content_type: &'static str,
    },
    /// Redirect (307) to this path: the build has the page under its other
    /// trailing-slash spelling.
    Redirect { location: String },
    /// Serve the SPA fallback file with 200: the route exists only in the
    /// client-side router.
    Fallback { path: PathBuf },
    /// There is nothing to serve — not even a fallback, so there is no
    /// Companion build in the configured directory.
    NotFound,
}

/// The Companion's build directory and the name of its SPA fallback file.
#[derive(Debug, Clone)]
pub struct Resolver {
    root: PathBuf,
    fallback: String,
}

impl Resolver {
    pub fn new(root: PathBuf, fallback: String) -> Self {
        Self { root, fallback }
    }

    /// The file the fallback would serve, whether or not it exists: what the
    /// startup check warns about.
    pub fn fallback_path(&self) -> PathBuf {
        self.root.join(&self.fallback)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a request path (still percent-encoded, as it arrives on the
    /// wire).
    pub async fn resolve(&self, request_path: &str) -> Resolution {
        let trailing_slash = request_path.ends_with('/');
        let Some(relative) = safe_relative_path(request_path) else {
            // A path that escapes the directory (or hides a NUL) is not a
            // path into the build: treat it as an unknown route.
            return self.fallback().await;
        };

        // 1. The exact file.
        if !relative.as_os_str().is_empty() && !trailing_slash {
            let candidate = self.root.join(&relative);
            if is_file(&candidate).await {
                return Resolution::File {
                    path: candidate,
                    content_type: content_type_for(request_path),
                };
            }
        }

        // 2. The prerendered page under the spelling this path asks for.
        let derived = if trailing_slash || relative.as_os_str().is_empty() {
            self.root.join(&relative).join("index.html")
        } else {
            let mut name = relative.as_os_str().to_owned();
            name.push(".html");
            self.root.join(PathBuf::from(name))
        };
        if is_file(&derived).await {
            return Resolution::File {
                path: derived,
                content_type: content_type_for(request_path),
            };
        }

        // 3 and 4. The build has the page under the other spelling.
        if trailing_slash && !relative.as_os_str().is_empty() {
            let mut name = relative.as_os_str().to_owned();
            name.push(".html");
            if is_file(&self.root.join(PathBuf::from(name))).await {
                return Resolution::Redirect {
                    location: request_path.trim_end_matches('/').to_owned(),
                };
            }
        }
        if !trailing_slash
            && !relative.as_os_str().is_empty()
            && is_file(&self.root.join(&relative).join("index.html")).await
        {
            return Resolution::Redirect {
                location: format!("{request_path}/"),
            };
        }

        // 6. A client-side route: the app shell answers it.
        self.fallback().await
    }

    async fn fallback(&self) -> Resolution {
        let path = self.fallback_path();
        if is_file(&path).await {
            Resolution::Fallback { path }
        } else {
            Resolution::NotFound
        }
    }
}

async fn is_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

/// The request path as a relative path inside the build directory, or `None`
/// when it is not one: a component that climbs out of the directory, an
/// absolute component, or a byte no file name may carry.
fn safe_relative_path(request_path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for segment in request_path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        let decoded = percent_decode(segment)?;
        if decoded.is_empty()
            || decoded == "."
            || decoded == ".."
            || decoded.contains('/')
            || decoded.contains('\\')
            || decoded.contains('\0')
        {
            return None;
        }
        out.push(decoded);
    }
    Some(out)
}

/// Percent-decodes one path segment, rejecting a malformed escape or a
/// sequence that is not UTF-8 — the Companion's build has no such file names.
fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = segment.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The content type of a request path, from its extension. Unknown and
/// missing extensions are `text/html`: an extension-less path is a page.
///
/// `.wasm` is exactly `application/wasm`, with no parameters. The Companion
/// loads the Matrix crypto WebAssembly through
/// `WebAssembly.instantiateStreaming`, which rejects any other or missing
/// MIME type with a `TypeError` and has no fallback path — a `charset`
/// parameter alone breaks onboarding, with an error that looks nothing like
/// a MIME problem.
pub fn content_type_for(request_path: &str) -> &'static str {
    let name = request_path.rsplit('/').next().unwrap_or_default();
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("wasm") => "application/wasm",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("html") | None => "text/html; charset=utf-8",
        _ => "text/html; charset=utf-8",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build directory shaped like a SvelteKit static export: a prerendered
    /// homepage, a prerendered nested page, one under a trailing-slash
    /// spelling, an asset, and the SPA fallback.
    fn build() -> tempdir::TempDir {
        let dir = tempdir::TempDir::new();
        dir.write("index.html", "<title>home</title>");
        dir.write("200.html", "<title>shell</title>");
        dir.write("onboarding/whatsapp.html", "<title>whatsapp</title>");
        dir.write("onboarding/signal/index.html", "<title>signal</title>");
        dir.write("_app/immutable/crypto.wasm", "\0asm");
        dir
    }

    fn resolver(dir: &tempdir::TempDir) -> Resolver {
        Resolver::new(dir.path().to_owned(), "200.html".to_owned())
    }

    #[tokio::test]
    async fn an_exact_file_wins() {
        let dir = build();
        let resolved = resolver(&dir).resolve("/_app/immutable/crypto.wasm").await;
        assert_eq!(
            resolved,
            Resolution::File {
                path: dir.path().join("_app/immutable/crypto.wasm"),
                content_type: "application/wasm",
            }
        );
    }

    #[tokio::test]
    async fn the_root_serves_the_prerendered_homepage_not_the_fallback() {
        let dir = build();
        assert_eq!(
            resolver(&dir).resolve("/").await,
            Resolution::File {
                path: dir.path().join("index.html"),
                content_type: "text/html; charset=utf-8",
            }
        );
    }

    #[tokio::test]
    async fn a_prerendered_page_resolves_through_its_html_file() {
        let dir = build();
        assert_eq!(
            resolver(&dir).resolve("/onboarding/whatsapp").await,
            Resolution::File {
                path: dir.path().join("onboarding/whatsapp.html"),
                content_type: "text/html; charset=utf-8",
            }
        );
    }

    #[tokio::test]
    async fn a_trailing_slash_page_resolves_through_its_index_file() {
        let dir = build();
        assert_eq!(
            resolver(&dir).resolve("/onboarding/signal/").await,
            Resolution::File {
                path: dir.path().join("onboarding/signal/index.html"),
                content_type: "text/html; charset=utf-8",
            }
        );
    }

    #[tokio::test]
    async fn each_spelling_redirects_to_the_one_the_build_has() {
        let dir = build();
        // The build has onboarding/whatsapp.html, the request has the slash.
        assert_eq!(
            resolver(&dir).resolve("/onboarding/whatsapp/").await,
            Resolution::Redirect {
                location: "/onboarding/whatsapp".to_owned()
            }
        );
        // The build has onboarding/signal/index.html, the request has none.
        assert_eq!(
            resolver(&dir).resolve("/onboarding/signal").await,
            Resolution::Redirect {
                location: "/onboarding/signal/".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn a_client_side_route_gets_the_fallback() {
        let dir = build();
        assert_eq!(
            resolver(&dir)
                .resolve("/contacts/%40alice%3Atest.twalk")
                .await,
            Resolution::Fallback {
                path: dir.path().join("200.html")
            }
        );
    }

    #[tokio::test]
    async fn no_build_at_all_resolves_to_nothing() {
        let dir = tempdir::TempDir::new();
        assert_eq!(resolver(&dir).resolve("/").await, Resolution::NotFound);
        assert_eq!(
            resolver(&dir).resolve("/anything").await,
            Resolution::NotFound
        );
    }

    #[tokio::test]
    async fn a_path_that_climbs_out_of_the_build_serves_the_fallback() {
        let dir = build();
        for escape in [
            "/../../etc/passwd",
            "/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
            "/onboarding/../../etc/passwd",
            "/%2fetc%2fpasswd",
        ] {
            assert_eq!(
                resolver(&dir).resolve(escape).await,
                Resolution::Fallback {
                    path: dir.path().join("200.html")
                },
                "{escape}"
            );
        }
    }

    #[test]
    fn the_wasm_content_type_carries_no_parameters() {
        // WebAssembly.instantiateStreaming accepts exactly this value.
        assert_eq!(content_type_for("/_app/crypto_bg.wasm"), "application/wasm");
        assert_eq!(content_type_for("/x/y.WASM"), "application/wasm");
    }

    #[test]
    fn content_types_come_from_the_extension_and_default_to_html() {
        assert_eq!(content_type_for("/app.css"), "text/css; charset=utf-8");
        assert_eq!(
            content_type_for("/_app/immutable/entry.abc123.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type_for("/manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(
            content_type_for("/onboarding/whatsapp"),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for("/weird.unknownext"),
            "text/html; charset=utf-8"
        );
    }

    /// A throwaway directory for the resolver's tests: the crate has no dev
    /// dependency for this, and the need is three lines deep.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                let unique = format!(
                    "twalk-gateway-resolver-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                let path = std::env::temp_dir().join(unique);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).expect("the temp directory is writable");
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }

            pub fn write(&self, relative: &str, contents: &str) {
                let path = self.0.join(relative);
                std::fs::create_dir_all(path.parent().expect("a file has a parent"))
                    .expect("the temp directory is writable");
                std::fs::write(path, contents).expect("the temp directory is writable");
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
