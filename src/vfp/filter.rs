//! Varnish Fetch Processor that buffers a backend response, minifies it, and
//! emits the minified bytes downstream.
//!
//! ## Behavior summary
//!
//! 1. At init, the filter reads `Content-Type` and `Cache-Control` from the
//!    `beresp` headers. It returns `InitResult::Pass` (filter never engages —
//!    zero per-byte cost) when:
//!    - Content-Type isn't a type we know how to minify, **or**
//!    - `Cache-Control` declares the response uncacheable (`no-store` or
//!      `private`). Minifying bytes that won't be cached is pure waste:
//!      we'd pay the buffer-then-process latency on every request without
//!      ever amortizing it across cache hits.
//!
//!    We deliberately do **not** gate on Content-Length: gzipped responses
//!    report the *compressed* size in Content-Length, which would falsely
//!    trigger a "too small" rejection even though the gunzipped body
//!    flowing through this filter is much larger. Tiny bodies still flow
//!    through cheaply — the dispatcher's size-guard returns the input
//!    verbatim when minification doesn't shrink it.
//!
//! 2. Otherwise, it enters `Buffering`: each `pull` call drains upstream into
//!    an internal `Vec<u8>` (sized from `Content-Length` when available)
//!    until upstream signals `End`. Then it runs the content-type dispatcher
//!    and transitions to `Draining`.
//!
//! 3. `Draining` emits the (possibly minified) bytes through subsequent
//!    `pull` calls. When the cursor reaches the end, returns `End(n)`.
//!
//! ## Latency note
//!
//! VFPs feed the same byte stream to both Varnish's storage *and* (with
//! `beresp.do_stream = true`, the default) the originating client. There is
//! no in-Varnish way to tee — let the client see original bytes while
//! buffering for cache. So buffer-then-process always defers TTFB on the
//! initial cache miss by the time it takes the backend to finish responding.
//! Subsequent cache hits don't run VFPs and are unaffected. With
//! `Cache-Control: stale-while-revalidate`, even refreshes are amortized:
//! the stale cached object ships immediately while a background fetch pays
//! the minify cost.
//!
//! ## Failure modes — never breaks the response
//!
//! - Optimizer error / parse error → drain *original* buffered bytes.
//! - Minified output ≥ original → drain original (the size-guard).
//! - Buffer would exceed `HARD_CAP_BYTES` (runtime safety net for misbehaving
//!   backends or absurd response sizes) → return `PullResult::Err`. The
//!   failure is bounded: Varnish marks the fetch failed; better than OOMing
//!   the host.

use std::ffi::CStr;

use varnish::vcl::{Ctx, FetchProcCtx, FetchProcessor, InitResult, PullResult};

use crate::vfp::dispatch::{applies_to, optimize_by_content_type};

/// Default initial buffer capacity when Content-Length isn't available
/// (chunked responses). Sized to fit the median dynamic HTML page in one
/// allocation without being wasteful on small bodies.
const DEFAULT_BUFFER_CAPACITY: usize = 64 * 1024;

/// Runtime safety net — if a misbehaving backend streams an unbounded
/// response, abort rather than letting the buffer grow without limit.
/// Generous enough to comfortably hold real-world HTML/JS bundles.
const HARD_CAP_BYTES: usize = 16 * 1024 * 1024;

/// Per-pull scratch buffer: how many bytes we ask upstream for at a time.
const SCRATCH_BYTES: usize = 64 * 1024;

const FILTER_NAME: &CStr = c"impress_minify";

/// Trait extracted to make the state machine unit-testable without Varnish.
/// `FetchProcCtx` implements it via [`FetchProcCtx::pull`]; tests use a fake.
pub(crate) trait BytePuller {
    fn pull(&mut self, buf: &mut [u8]) -> PullResult;
}

impl BytePuller for FetchProcCtx<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> PullResult {
        FetchProcCtx::pull(self, buf)
    }
}

enum State {
    Buffering { buf: Vec<u8>, content_type: String },
    Draining { bytes: Vec<u8>, cursor: usize },
}

pub struct MinifyVfp {
    state: State,
    /// Reused per-pull scratch — heap-allocated once, avoids re-zeroing 64 KiB
    /// of stack on every `pull`. Stored in a `Box` so the `MinifyVfp` itself
    /// (which Varnish boxes) stays tiny.
    scratch: Box<[u8; SCRATCH_BYTES]>,
}

impl MinifyVfp {
    fn new(content_type: String, expected_len: usize) -> Self {
        Self {
            state: State::Buffering {
                buf: Vec::with_capacity(expected_len),
                content_type,
            },
            scratch: Box::new([0u8; SCRATCH_BYTES]),
        }
    }

    /// Pure state-machine step — generic over a `BytePuller` so tests can
    /// drive it with canned upstream behavior. The real VFP delegates here
    /// after wrapping the `FetchProcCtx`.
    pub(crate) fn step<P: BytePuller>(&mut self, upstream: &mut P, out: &mut [u8]) -> PullResult {
        loop {
            match &mut self.state {
                State::Buffering { buf, content_type } => {
                    let scratch: &mut [u8] = self.scratch.as_mut();
                    match upstream.pull(scratch) {
                        PullResult::Err => return PullResult::Err,
                        PullResult::Ok(n) => {
                            if buf.len().saturating_add(n) > HARD_CAP_BYTES {
                                return PullResult::Err;
                            }
                            buf.extend_from_slice(&scratch[..n]);
                        }
                        PullResult::End(n) => {
                            if buf.len().saturating_add(n) > HARD_CAP_BYTES {
                                return PullResult::Err;
                            }
                            buf.extend_from_slice(&scratch[..n]);
                            //Run the optimizer; on any failure (None) we keep
                            //the original buffered bytes — never break the
                            //response just because we couldn't shrink it.
                            let bytes = optimize_by_content_type(buf, content_type)
                                .unwrap_or_else(|| std::mem::take(buf));
                            self.state = State::Draining { bytes, cursor: 0 };
                            //fall through into the Draining arm via loop
                        }
                    }
                }
                State::Draining { bytes, cursor } => {
                    let remaining = bytes.len() - *cursor;
                    if remaining == 0 {
                        return PullResult::End(0);
                    }
                    let n = remaining.min(out.len());
                    out[..n].copy_from_slice(&bytes[*cursor..*cursor + n]);
                    *cursor += n;
                    return if *cursor == bytes.len() {
                        PullResult::End(n)
                    } else {
                        PullResult::Ok(n)
                    };
                }
            }
        }
    }
}

impl FetchProcessor for MinifyVfp {
    fn name() -> &'static CStr {
        FILTER_NAME
    }

    fn new(ctx: &mut Ctx, _vfp_ctx: &mut FetchProcCtx) -> InitResult<Self> {
        //Phase 1 — read-only inspection. We collect everything we need from
        //beresp headers up-front so we can release the immutable borrow
        //before mutating headers in phase 2.
        let (ct, cl_hint) = {
            let beresp = match ctx.http_beresp.as_ref() {
                Some(h) => h,
                None => return InitResult::Pass,
            };

            let ct = match beresp.header("content-type") {
                Some(v) => match std::str::from_utf8(v.as_ref()) {
                    Ok(s) => s.to_owned(),
                    Err(_) => return InitResult::Pass,
                },
                None => return InitResult::Pass,
            };
            if !applies_to(&ct) {
                return InitResult::Pass;
            }

            //Skip the filter when the response is declared uncacheable —
            //there's no point spending CPU minifying bytes Varnish will
            //throw away right after delivery. We only catch directives that
            //unambiguously mean "do not cache in shared caches"; anything
            //else (no-cache, max-age=0, …) is left to user VCL.
            if let Some(cc) = beresp.header("cache-control") {
                if let Ok(s) = std::str::from_utf8(cc.as_ref()) {
                    let lower = s.to_ascii_lowercase();
                    if lower.contains("no-store") || lower.contains("private") {
                        return InitResult::Pass;
                    }
                }
            }

            //Content-Length serves only as a capacity hint here, capped so
            //a wildly large header doesn't trigger a single huge alloc.
            let cl_hint = match beresp.header("content-length") {
                Some(v) => std::str::from_utf8(v.as_ref())
                    .ok()
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(DEFAULT_BUFFER_CAPACITY),
                None => DEFAULT_BUFFER_CAPACITY,
            }
            .min(HARD_CAP_BYTES);

            (ct, cl_hint)
        };

        //Phase 2 — strip Content-Length. The original CL describes the
        //un-minified body and would let downstream clients truncate at the
        //wrong byte once we shrink the response. Varnish falls back to
        //chunked transfer-encoding (or recomputes CL from the cached
        //object's actual size) when the header is absent.
        if let Some(beresp) = ctx.http_beresp.as_mut() {
            beresp.unset_header("content-length");
        }

        InitResult::Ok(MinifyVfp::new(ct, cl_hint))
    }

    fn pull(&mut self, ctx: &mut FetchProcCtx, out: &mut [u8]) -> PullResult {
        self.step(ctx, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned upstream — replays a sequence of `PullResult`s back to the state
    /// machine. Each entry pairs a result tag with the bytes to write into the
    /// caller's scratch.
    struct FakePuller {
        chunks: std::collections::VecDeque<(Vec<u8>, ResultTag)>,
    }

    enum ResultTag {
        Ok,
        End,
        Err,
    }

    impl FakePuller {
        fn new(chunks: Vec<(Vec<u8>, ResultTag)>) -> Self {
            Self { chunks: chunks.into() }
        }
    }

    impl BytePuller for FakePuller {
        fn pull(&mut self, buf: &mut [u8]) -> PullResult {
            let (data, tag) = match self.chunks.pop_front() {
                Some(c) => c,
                //Exhausted; tests that miscount their inputs surface as a panic
                //rather than a silent infinite loop.
                None => panic!("FakePuller exhausted: state machine asked for more"),
            };
            assert!(data.len() <= buf.len(), "test data chunk wider than scratch");
            buf[..data.len()].copy_from_slice(&data);
            match tag {
                ResultTag::Ok => PullResult::Ok(data.len()),
                ResultTag::End => PullResult::End(data.len()),
                ResultTag::Err => PullResult::Err,
            }
        }
    }

    fn drain<P: BytePuller>(vfp: &mut MinifyVfp, p: &mut P) -> Result<Vec<u8>, ()> {
        let mut acc = Vec::new();
        let mut out = vec![0u8; 4096];
        loop {
            match vfp.step(p, &mut out) {
                PullResult::Ok(n) => acc.extend_from_slice(&out[..n]),
                PullResult::End(n) => {
                    acc.extend_from_slice(&out[..n]);
                    return Ok(acc);
                }
                PullResult::Err => return Err(()),
            }
        }
    }

    #[test]
    fn end_to_end_minifies_buffered_html() {
        let body = b"<html>\n  <body>\n    <p>hi</p>\n  </body>\n</html>".to_vec();
        let mut vfp = MinifyVfp::new("text/html".into(), body.len());
        let mut up = FakePuller::new(vec![(body.clone(), ResultTag::End)]);
        let out = drain(&mut vfp, &mut up).unwrap();
        assert!(out.len() < body.len(), "got {} bytes, expected <{}", out.len(), body.len());
        assert!(std::str::from_utf8(&out).unwrap().contains("<p>hi"));
    }

    #[test]
    fn handles_partial_chunks_before_end() {
        //Upstream sends the body in three pieces; result must equal the
        //single-chunk minification.
        let body: &[u8] = b"<html>\n  <body>\n    <p>hello world</p>\n  </body>\n</html>";
        let split = (body.len() / 3, 2 * body.len() / 3);
        let chunks = vec![
            (body[..split.0].to_vec(), ResultTag::Ok),
            (body[split.0..split.1].to_vec(), ResultTag::Ok),
            (body[split.1..].to_vec(), ResultTag::End),
        ];
        let mut vfp = MinifyVfp::new("text/html".into(), body.len());
        let mut up = FakePuller::new(chunks);
        let out = drain(&mut vfp, &mut up).unwrap();
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("<p>hello world"));
        assert!(out.len() < body.len());
    }

    #[test]
    fn unsupported_content_type_returns_input_verbatim() {
        //If the dispatcher returns None, the buffered bytes flow through
        //unchanged. (In production this is gated at init; the state machine
        //must still behave correctly if reached.)
        let body = b"\x00\x01raw bytes\x02".to_vec();
        let mut vfp = MinifyVfp::new("application/octet-stream".into(), body.len());
        let mut up = FakePuller::new(vec![(body.clone(), ResultTag::End)]);
        assert_eq!(drain(&mut vfp, &mut up).unwrap(), body);
    }

    #[test]
    fn invalid_html_returns_buffered_bytes() {
        //minify-html is permissive — but the size-guard ensures we never
        //grow the body. We don't assert exact bytes, only the invariants.
        let body = b"<<< not html >>>".to_vec();
        let mut vfp = MinifyVfp::new("text/html".into(), body.len());
        let mut up = FakePuller::new(vec![(body.clone(), ResultTag::End)]);
        let out = drain(&mut vfp, &mut up).unwrap();
        assert!(out.len() <= body.len());
    }

    #[test]
    fn upstream_error_propagates() {
        let mut vfp = MinifyVfp::new("text/html".into(), 0);
        let mut up = FakePuller::new(vec![(vec![], ResultTag::Err)]);
        assert!(drain(&mut vfp, &mut up).is_err());
    }

    #[test]
    fn empty_body_emits_end_zero() {
        let mut vfp = MinifyVfp::new("text/html".into(), 0);
        let mut up = FakePuller::new(vec![(vec![], ResultTag::End)]);
        let out = drain(&mut vfp, &mut up).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn drain_handles_small_out_buffer() {
        //Caller passes a tiny output buffer; we must split the drain across
        //multiple pull calls without losing or duplicating bytes.
        let body: &[u8] = b"<html>\n  <body>\n    <p>hello world hello world</p>\n  </body>\n</html>";
        let mut vfp = MinifyVfp::new("text/html".into(), body.len());
        let mut up = FakePuller::new(vec![(body.to_vec(), ResultTag::End)]);

        //Manually drain in 4-byte slices.
        let mut acc = Vec::new();
        let mut out = [0u8; 4];
        loop {
            match vfp.step(&mut up, &mut out) {
                PullResult::Ok(n) => acc.extend_from_slice(&out[..n]),
                PullResult::End(n) => {
                    acc.extend_from_slice(&out[..n]);
                    break;
                }
                PullResult::Err => panic!("unexpected err"),
            }
        }
        let s = std::str::from_utf8(&acc).unwrap();
        assert!(s.contains("<p>hello world hello world"));
    }

    #[test]
    fn runtime_buffer_overflow_returns_err() {
        //Apache claimed CL=100 but keeps streaming past HARD_CAP_BYTES.
        //We must abort rather than OOM the host.
        let big = vec![b'x'; SCRATCH_BYTES];
        let mut chunks = Vec::new();
        let needed = HARD_CAP_BYTES / SCRATCH_BYTES + 2;
        for _ in 0..needed {
            chunks.push((big.clone(), ResultTag::Ok));
        }
        let mut vfp = MinifyVfp::new("text/html".into(), 100);
        let mut up = FakePuller::new(chunks);
        let mut out = vec![0u8; 4096];
        let mut saw_err = false;
        for _ in 0..(needed + 4) {
            if matches!(vfp.step(&mut up, &mut out), PullResult::Err) {
                saw_err = true;
                break;
            }
        }
        assert!(saw_err, "should abort once HARD_CAP_BYTES is exceeded");
    }
}
