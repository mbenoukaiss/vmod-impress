//! Content-Type → optimizer dispatch for the on-the-fly minifier.
//!
//! Mirrors `static_files::optimizer::dispatch_by_ext`, but keyed on the
//! HTTP Content-Type header (with optional `;charset=…` parameter stripped).
//!
//! Returns `None` to mean "pass through unchanged" — both for unknown types
//! and for cases where minification didn't make the payload smaller. Callers
//! avoid an allocation in that path.

use crate::static_files::optimizer;

fn bare(ct: &str) -> &str {
    ct.split(';').next().unwrap_or("").trim()
}

/// Optimize `input` based on Content-Type. Returns:
/// * `Some(bytes)` — minified output, strictly smaller than input.
/// * `None` — passthrough: type unsupported, optimizer errored, or output ≥ input.
pub fn optimize_by_content_type(input: &[u8], ct: &str) -> Option<Vec<u8>> {
    let kind = bare(ct).to_ascii_lowercase();
    let result = match kind.as_str() {
        "text/html" => optimizer::optimize_html(input),
        "text/css" => optimizer::optimize_css(input),
        "text/javascript" | "application/javascript" => optimizer::optimize_js(input),
        "application/json" => optimizer::optimize_json(input),
        _ => return None,
    };
    match result {
        Ok(out) if out.len() < input.len() => Some(out),
        _ => None,
    }
}

/// Cheap probe — does our dispatcher know how to minify this content-type?
/// Defense-in-depth complement to the VCL-side gate in `vcl_backend_response`.
pub fn applies_to(ct: &str) -> bool {
    matches!(
        bare(ct).to_ascii_lowercase().as_str(),
        "text/html"
            | "text/css"
            | "text/javascript"
            | "application/javascript"
            | "application/json"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_dispatches_to_html_minifier() {
        //note: minify-html drops optional close tags, so `</p>` may be
        //gone even though the visible content is identical — assert on
        //the visible text rather than tag punctuation.
        let input = b"<html>\n  <body>\n    <p>hi</p>\n  </body>\n</html>";
        let out = optimize_by_content_type(input, "text/html; charset=utf-8").unwrap();
        assert!(out.len() < input.len());
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.contains("<p>hi"),
            "tag + visible content preserved: {s:?}"
        );
        assert!(
            !s.contains("\n  "),
            "indentation should be collapsed: {s:?}"
        );
    }

    #[test]
    fn json_dispatches_to_json_minifier() {
        let input = br#"{ "a": 1 }"#;
        let out = optimize_by_content_type(input, "application/json").unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn css_dispatches() {
        let input = b".foo { color:  red ; padding: 0px;  }";
        let out = optimize_by_content_type(input, "text/css").unwrap();
        assert!(out.len() < input.len());
        assert!(std::str::from_utf8(&out).unwrap().contains("red"));
    }

    #[test]
    fn js_dispatches_via_text_javascript() {
        let input = b"const longName = 1 + 2;\nconst other = longName + 3;\nconsole.log(other);\n";
        let out = optimize_by_content_type(input, "text/javascript").unwrap();
        assert!(out.len() < input.len());
    }

    #[test]
    fn js_dispatches_via_application_javascript() {
        let input = b"const longName = 1 + 2;\nconst other = longName + 3;\nconsole.log(other);\n";
        let out = optimize_by_content_type(input, "application/javascript").unwrap();
        assert!(out.len() < input.len());
    }

    #[test]
    fn unknown_content_type_returns_none() {
        assert!(optimize_by_content_type(b"\x00binary", "image/png").is_none());
    }

    #[test]
    fn svg_is_not_dispatched() {
        //SVG isn't optimized by this VMOD (oxvg/lightningcss feature conflict);
        //treat it like any other unknown type.
        assert!(optimize_by_content_type(b"<svg/>", "image/svg+xml").is_none());
    }

    #[test]
    fn text_plain_is_not_dispatched() {
        assert!(optimize_by_content_type(b"hello world hello world", "text/plain").is_none());
    }

    #[test]
    fn no_op_passthrough_returns_none_not_a_copy() {
        //Tiny input where minify can't shrink — we must NOT return Some(copy_of_input).
        //None signals "use the input verbatim", saving the allocation downstream.
        let input = b"{}";
        assert!(optimize_by_content_type(input, "application/json").is_none());
    }

    #[test]
    fn handles_charset_parameters() {
        let input = b".foo { color: red; padding: 5px; margin: 5px; }".repeat(8);
        let with = optimize_by_content_type(&input, "text/css; charset=utf-8").unwrap();
        let without = optimize_by_content_type(&input, "text/css").unwrap();
        assert_eq!(with, without);
    }

    #[test]
    fn case_insensitive_content_type() {
        let input = b"<html>\n  <body>\n    <p>hi</p>\n  </body>\n</html>";
        let upper = optimize_by_content_type(input, "TEXT/HTML").unwrap();
        let lower = optimize_by_content_type(input, "text/html").unwrap();
        assert_eq!(upper, lower);
    }

    #[test]
    fn applies_to_recognises_supported_types() {
        assert!(applies_to("text/html"));
        assert!(applies_to("text/html; charset=utf-8"));
        assert!(applies_to("TEXT/HTML"));
        assert!(applies_to("text/css"));
        assert!(applies_to("text/javascript"));
        assert!(applies_to("application/javascript"));
        assert!(applies_to("application/json"));
    }

    #[test]
    fn applies_to_excludes_unsupported_types() {
        assert!(!applies_to("image/png"));
        assert!(!applies_to("image/svg+xml"));
        assert!(!applies_to("font/woff2"));
        assert!(!applies_to("application/octet-stream"));
        assert!(!applies_to("text/plain"));
        assert!(!applies_to(""));
    }
}
