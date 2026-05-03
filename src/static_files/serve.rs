use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::backend::FileTransfer;
use crate::cache::{format_http_date, FetchResult};
use crate::config::Config;
use crate::error::Error;
use crate::static_files::{optimizer, security, MemoryTransfer, Transfer};

/// Bumped manually whenever an optimizer crate upgrade changes output bytes
/// for unchanged sources. Mixing this into the etag means clients holding an
/// `If-None-Match` from before the bump get a real 200 with the new bytes,
/// not a stale 304.
const STATIC_OPTIMIZER_VERSION: u64 = 1;

pub fn serve(config: &Config, route_id: usize, rel_path: &str) -> Result<Option<FetchResult>, Error> {
    let route = config.statics.get(route_id)
        .ok_or_else(|| Error::new(format!("unknown route id {route_id}")))?;
    let root_canon = route.root_canon.as_ref()
        .ok_or_else(|| Error::new("route root_canon not initialized"))?;

    let source = match security::safe_join(root_canon, rel_path) {
        Some(p) => p,
        None => return Ok(None),
    };

    let ext = Path::new(rel_path)
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mime = mime_for_ext(&ext);

    let mut file = File::open(&source)?;
    let meta = file.metadata()?;
    //safe_join's canonicalize succeeds on directories too — we have to gate
    //that here, otherwise File::open would have already errored with
    //IsADirectory which the backend surfaces as 500 (not 404)
    if !meta.is_file() {
        return Ok(None);
    }
    let last_modified: DateTime<Utc> = DateTime::from(meta.modified()?);
    let inode = meta.ino();
    let file_len = meta.len();
    let mtime_secs = last_modified.timestamp();

    let optimization_enabled = match ext.as_str() {
        "html" | "htm" => route.optimization.html,
        "css" => route.optimization.css,
        "js" | "mjs" => route.optimization.js,
        "json" => route.optimization.json,
        _ => false,
    };
    let size_ok = route.allows_optimization_at_size(file_len as usize);
    let want_optimize = optimizer::applies_to_ext(&ext) && optimization_enabled && size_ok;

    if !want_optimize {
        return Ok(Some(build_fetch_result(
            Transfer::File(FileTransfer::new(file, file_len)),
            file_len,
            last_modified,
            inode,
            mtime_secs,
            mime,
            false,
            route.cache_control_value.clone(),
        )));
    }

    //Optimization needs the whole document resident — every supported optimizer
    //(minify-html, lightningcss, oxc_minifier, serde_json) buffers internally.
    //We read from the same fd we stat'd above, so the bytes we minify match the
    //metadata in the etag: a rename/replace between stat and read can't slip a
    //different file's contents through with the original file's inode+mtime+size.
    //The bytes stay in heap and we ship via MemoryTransfer whether dispatch
    //shrank them or not — no second open of the path (no TOCTOU window).
    let mut bytes = Vec::with_capacity(file_len as usize);
    file.read_to_end(&mut bytes)?;
    drop(file);
    let optimized = optimizer::dispatch_by_ext(&bytes, &ext)?;
    let is_optimized = optimized.len() < bytes.len();
    let body_len = optimized.len() as u64;

    Ok(Some(build_fetch_result(
        Transfer::Memory(MemoryTransfer::new(optimized)),
        body_len,
        last_modified,
        inode,
        mtime_secs,
        mime,
        is_optimized,
        route.cache_control_value.clone(),
    )))
}

#[allow(clippy::too_many_arguments)]
fn build_fetch_result(
    data: Transfer,
    body_len: u64,
    last_modified: DateTime<Utc>,
    inode: u64,
    mtime_secs: i64,
    mime: &'static str,
    is_optimized: bool,
    cache_control: Arc<str>,
) -> FetchResult {
    let last_modified_str = format_http_date(last_modified);
    let content_length_str = body_len.to_string();
    //etag mixes inode+mtime+size for source identity, plus mime so two
    //files at the same inode (e.g. inode reuse after rename) with different
    //extensions get different etags, plus a bool for the optimization
    //outcome and a version constant so upgrades that change optimizer
    //output bytes invalidate the cache automatically
    let etag = static_etag(inode, body_len, mtime_secs, mime, is_optimized);
    FetchResult {
        data,
        last_modified,
        last_modified_str: Arc::from(last_modified_str.as_str()),
        etag: Arc::from(etag.as_str()),
        content_length_str: Arc::from(content_length_str.as_str()),
        mime,
        is_optimized,
        cache_control,
    }
}

fn static_etag(inode: u64, size: u64, mtime_secs: i64, mime: &str, is_optimized: bool) -> String {
    let mut h = DefaultHasher::new();
    (inode, size, mtime_secs, mime, is_optimized, STATIC_OPTIMIZER_VERSION).hash(&mut h);
    format!("\"{}\"", h.finish())
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "wasm" => "application/wasm",
        "map" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use crate::config::{Optimization, StaticRoute};

    fn make_config(static_root: &std::path::Path, optimization: Optimization, max_bytes: Option<usize>) -> Config {
        Config {
            statics: vec![StaticRoute {
                url: "/x/{path}".into(),
                root: static_root.to_string_lossy().to_string(),
                cache_control: None,
                optimization,
                optimize_max_bytes: max_bytes,
                url_regex: None,
                root_canon: Some(std::fs::canonicalize(static_root).unwrap()),
                cache_control_value: Arc::from("public, max-age=86400"),
            }],
            ..Config::default()
        }
    }

    fn write(path: &PathBuf, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    fn opt_all_on() -> Optimization {
        Optimization { html: true, css: true, js: true, json: true }
    }

    #[test]
    fn css_returns_minified_via_memory_transfer() {
        let src = TempDir::new().unwrap();
        let payload: Vec<u8> = b".foo {  color:  red ; }".repeat(50);
        write(&src.path().join("a.css"), &payload);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.css").unwrap().unwrap();
        assert!(result.is_optimized, "css should shrink");
        assert_eq!(result.mime, "text/css; charset=utf-8");
        assert!(matches!(result.data, Transfer::Memory(_)));
        assert!(result.data.size() < payload.len());
        assert_eq!(&*result.cache_control, "public, max-age=86400");
    }

    #[test]
    fn png_streams_from_disk_via_file_transfer() {
        let src = TempDir::new().unwrap();
        write(&src.path().join("a.png"), &[0u8; 1024]);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.png").unwrap().unwrap();
        assert!(!result.is_optimized);
        assert_eq!(result.mime, "image/png");
        assert!(matches!(result.data, Transfer::File(_)));
        assert_eq!(result.data.size(), 1024);
    }

    #[test]
    fn no_extension_streams_from_disk() {
        //ext-less file: dispatch isn't applied, served as octet-stream
        let src = TempDir::new().unwrap();
        write(&src.path().join("README"), b"hello");
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "README").unwrap().unwrap();
        assert!(!result.is_optimized);
        assert_eq!(result.mime, "application/octet-stream");
        assert!(matches!(result.data, Transfer::File(_)));
    }

    #[test]
    fn already_minified_css_still_uses_memory_transfer() {
        //Per fix #9: even when dispatch reports no-improvement, we ship the
        //bytes already in heap via MemoryTransfer rather than re-opening the
        //file (avoids the read-after-rename TOCTOU window).
        let src = TempDir::new().unwrap();
        let payload = b".a{color:#abc}";
        write(&src.path().join("a.css"), payload);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.css").unwrap().unwrap();
        assert!(matches!(result.data, Transfer::Memory(_)));
        assert!(!result.is_optimized);
    }

    #[test]
    fn rejects_traversal() {
        let src = TempDir::new().unwrap();
        let config = make_config(src.path(), opt_all_on(), None);
        assert!(serve(&config, 0, "../etc/passwd").unwrap().is_none());
    }

    #[test]
    fn missing_file_returns_none() {
        let src = TempDir::new().unwrap();
        let config = make_config(src.path(), opt_all_on(), None);
        assert!(serve(&config, 0, "doesnotexist.css").unwrap().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn symlink_to_file_inside_root_serves_it() {
        //safe_join's canonicalize resolves symlinks, so a symlink that
        //points at a regular file *inside* the root must still serve.
        //Only escapes (resolved target outside root) should 404.
        let src = TempDir::new().unwrap();
        let real = src.path().join("real.css");
        std::fs::write(&real, b".a{color:red}").unwrap();
        std::os::unix::fs::symlink(&real, src.path().join("alias.css")).unwrap();
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "alias.css").unwrap()
            .expect("symlink-to-internal-file should serve");
        assert_eq!(result.mime, "text/css; charset=utf-8");
    }

    #[test]
    fn directory_returns_none() {
        //if rel_path resolves to a directory inside the root, serve must
        //treat it as 404 rather than letting File::open succeed and then
        //blowing up downstream when we try to stat as a regular file
        let src = TempDir::new().unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        let config = make_config(src.path(), opt_all_on(), None);
        assert!(serve(&config, 0, "sub").unwrap().is_none());
    }

    #[test]
    fn etag_stable_for_same_source() {
        let src = TempDir::new().unwrap();
        write(&src.path().join("a.css"), b".foo { color: red }");
        let config = make_config(src.path(), opt_all_on(), None);

        let r1 = serve(&config, 0, "a.css").unwrap().unwrap();
        let r2 = serve(&config, 0, "a.css").unwrap().unwrap();
        assert_eq!(r1.etag, r2.etag);
    }

    #[test]
    fn etag_changes_on_mtime() {
        let src = TempDir::new().unwrap();
        let path = src.path().join("a.css");
        write(&path, b".foo { color: red }");
        let config = make_config(src.path(), opt_all_on(), None);

        let r1 = serve(&config, 0, "a.css").unwrap().unwrap();
        //touch with a future mtime so the etag input changes deterministically
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let f = std::fs::File::open(&path).unwrap();
        f.set_modified(future).unwrap();
        let r2 = serve(&config, 0, "a.css").unwrap().unwrap();
        assert_ne!(r1.etag, r2.etag);
    }

    #[test]
    fn optimize_max_bytes_zero_disables_cap() {
        //The docstring promises `optimize_max_bytes: Some(0)` disables the
        //cap entirely. Make sure that contract holds — without this test a
        //refactor could swap the `||` for an `&&` and silently default to
        //"never optimize" instead of "always optimize".
        let src = TempDir::new().unwrap();
        let payload: Vec<u8> = b".foo {  color:  red ; }".repeat(1000);
        write(&src.path().join("a.css"), &payload);
        let config = make_config(src.path(), opt_all_on(), Some(0));

        let result = serve(&config, 0, "a.css").unwrap().unwrap();
        assert!(result.is_optimized);
        assert!(matches!(result.data, Transfer::Memory(_)));
    }

    #[test]
    fn streaming_path_uses_optimized_cache_control() {
        //Static routes have no in-flight semantic, so even the non-optimized
        //path should advertise the optimized Cache-Control header
        let src = TempDir::new().unwrap();
        write(&src.path().join("a.png"), &[0u8; 1024]);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.png").unwrap().unwrap();
        assert_eq!(&*result.cache_control, "public, max-age=86400");
    }

    #[test]
    fn etag_varies_on_mime() {
        //Two etags with the same source identity but different MIMEs must
        //differ — guards against inode reuse / file renaming where the
        //inode could repeat with a new extension
        let a = static_etag(42, 100, 1_700_000_000, "text/css; charset=utf-8", true);
        let b = static_etag(42, 100, 1_700_000_000, "application/json; charset=utf-8", true);
        assert_ne!(a, b);
    }

    #[test]
    fn etag_varies_on_optimizer_version_salt() {
        //If STATIC_OPTIMIZER_VERSION isn't actually mixed into the hash, this
        //test would compare two equal hashes (regression guard for someone
        //quietly dropping the salt)
        let v1 = static_etag(42, 100, 1_700_000_000, "text/css; charset=utf-8", true);
        let mut h = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(
            &(42u64, 100u64, 1_700_000_000_i64, "text/css; charset=utf-8", true, STATIC_OPTIMIZER_VERSION + 1),
            &mut h,
        );
        let v2_with_other_salt = format!("\"{}\"", std::hash::Hasher::finish(&h));
        assert_ne!(v1, v2_with_other_salt);
    }

    #[test]
    fn oversized_html_falls_back_to_streaming() {
        //Per fix #10: files larger than optimize_max_bytes skip the heavy
        //in-memory path and stream from disk
        let src = TempDir::new().unwrap();
        let big: Vec<u8> = b"<html>\n  <body>hi</body>\n</html>\n".repeat(50);
        write(&src.path().join("a.html"), &big);
        let config = make_config(src.path(), opt_all_on(), Some(64));

        let result = serve(&config, 0, "a.html").unwrap().unwrap();
        assert!(matches!(result.data, Transfer::File(_)),
                "oversized file should stream from disk, not load into memory");
        assert!(!result.is_optimized);
    }

    #[test]
    fn html_optimization_returns_smaller_memory_body() {
        let src = TempDir::new().unwrap();
        let payload = b"<html>\n  <head><title>hi</title></head>\n  <body>\n    <p>hi</p>\n  </body>\n</html>\n";
        write(&src.path().join("a.html"), payload);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.html").unwrap().unwrap();
        assert!(result.is_optimized);
        assert_eq!(result.mime, "text/html; charset=utf-8");
        assert!(matches!(result.data, Transfer::Memory(_)));
        assert!(result.data.size() < payload.len());
    }

    #[test]
    fn json_optimization_returns_smaller_memory_body() {
        let src = TempDir::new().unwrap();
        let payload = br#"{
            "key":  "value",
            "n":    42,
            "list": [1,    2,    3]
        }"#;
        write(&src.path().join("a.json"), payload);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.json").unwrap().unwrap();
        assert!(result.is_optimized);
        assert_eq!(result.mime, "application/json; charset=utf-8");
        assert!(matches!(result.data, Transfer::Memory(_)));
    }

    #[test]
    fn js_optimization_returns_smaller_memory_body() {
        let src = TempDir::new().unwrap();
        let payload = b"const longName = 1 + 2;\nconst other = longName + 3;\nconsole.log(other);\n";
        write(&src.path().join("a.js"), payload);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.js").unwrap().unwrap();
        assert!(result.is_optimized);
        assert_eq!(result.mime, "application/javascript; charset=utf-8");
        assert!(matches!(result.data, Transfer::Memory(_)));
    }

    #[test]
    fn woff2_streams_from_disk_with_correct_mime() {
        let src = TempDir::new().unwrap();
        write(&src.path().join("font.woff2"), &[0u8; 256]);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "font.woff2").unwrap().unwrap();
        assert!(!result.is_optimized);
        assert_eq!(result.mime, "font/woff2");
        assert!(matches!(result.data, Transfer::File(_)));
    }

    #[test]
    fn svg_streams_from_disk() {
        //SVG isn't optimized in this VMOD; should stream from disk
        let src = TempDir::new().unwrap();
        let payload = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        write(&src.path().join("a.svg"), payload);
        let config = make_config(src.path(), opt_all_on(), None);

        let result = serve(&config, 0, "a.svg").unwrap().unwrap();
        assert!(!result.is_optimized);
        assert_eq!(result.mime, "image/svg+xml");
        assert!(matches!(result.data, Transfer::File(_)));
    }

    #[test]
    fn etag_differs_for_optimized_vs_streamed_same_source() {
        //Same source, different optimization toggles → different etags. This
        //matters when a deployment flips optimization.css off then on:
        //clients holding a cached If-None-Match shouldn't receive a 304 with
        //bytes from the other variant.
        let src = TempDir::new().unwrap();
        let payload: Vec<u8> = b".foo {  color:  red ; }".repeat(50);
        write(&src.path().join("a.css"), &payload);
        let opt_on = opt_all_on();
        let opt_off = Optimization { html: true, css: false, js: true, json: true };
        let cfg_on = make_config(src.path(), opt_on, None);
        let cfg_off = make_config(src.path(), opt_off, None);

        let r_on = serve(&cfg_on, 0, "a.css").unwrap().unwrap();
        let r_off = serve(&cfg_off, 0, "a.css").unwrap().unwrap();
        assert!(r_on.is_optimized);
        assert!(!r_off.is_optimized);
        assert_ne!(r_on.etag, r_off.etag);
    }

    #[test]
    #[cfg(unix)]
    fn rename_after_open_serves_original_bytes() {
        //Regression: previously the optimize path did `drop(file)` and re-read
        //via std::fs::read(&source), so a rename between stat and re-read could
        //ship bytes from the new file with an etag from the old stat. We now
        //read from the same fd we stat'd. Verify by atomically replacing the
        //source file after we open it (via std::fs::rename of a sibling onto
        //the source path) — the Unix kernel keeps our fd attached to the
        //original inode, so we should still see the v1 bytes.
        use std::io::Write as _;
        let src = TempDir::new().unwrap();
        let path = src.path().join("a.css");
        write(&path, b".v1{color:red}.spacer{display:none}.spacer{display:none}.spacer{display:none}");

        //Race the rename in: we hold open the file in this thread, replace
        //the path with a sibling that has different content, and then run
        //serve. Since serve opens its own fd to the canonical path, simulating
        //"rename happens between serve's File::open and read_to_end" requires
        //a real injection point — instead, prove the kernel guarantee at a
        //lower level: a fd held across the rename keeps the original bytes.
        let mut held = std::fs::File::open(&path).unwrap();
        let new_payload = b".v2{color:blue}";
        let other = src.path().join("b.css");
        let mut o = std::fs::File::create(&other).unwrap();
        o.write_all(new_payload).unwrap();
        std::fs::rename(&other, &path).unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut held, &mut buf).unwrap();
        assert!(buf.contains("v1"), "fd held across rename must still read v1: got {buf:?}");

        //Now run serve against the post-rename path: it opens a fresh fd and
        //reads the new bytes from the same fd it stat'd, so etag and body are
        //consistent with each other.
        let config = make_config(src.path(), opt_all_on(), None);
        let result = serve(&config, 0, "a.css").unwrap().unwrap();
        assert!(matches!(result.data, Transfer::Memory(_)));
    }

    #[test]
    fn optimization_disabled_streams_from_disk() {
        let src = TempDir::new().unwrap();
        let payload: Vec<u8> = b".foo {  color:  red ; }".repeat(50);
        write(&src.path().join("a.css"), &payload);
        let opt = Optimization { html: true, css: false, js: true, json: true };
        let config = make_config(src.path(), opt, None);

        let result = serve(&config, 0, "a.css").unwrap().unwrap();
        assert!(matches!(result.data, Transfer::File(_)));
        assert!(!result.is_optimized);
    }
}
