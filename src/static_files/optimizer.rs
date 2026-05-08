use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::error::Error;

/// Run `f` with a panic catcher around it. minify-html, oxc_minifier (alpha),
/// lightningcss, and serde_json can all panic on pathological input. The
/// `varnish::vmod` macro doesn't catch_unwind across the FFI boundary
/// (Cargo.toml documents this), so an uncaught panic in either the static-file
/// hot path or the VFP would abort varnishd. We convert panics to `Err` so
/// `dispatch_by_ext`'s existing fall-through-to-input branch handles them
/// the same way as ordinary parse errors.
fn run_safely<F: FnOnce() -> Result<Vec<u8>, Error>>(name: &str, f: F) -> Result<Vec<u8>, Error> {
    //AssertUnwindSafe: closures over `&[u8]` are unwind-safe by construction;
    //the assertion is needed because `dyn FnOnce` is conservative about it.
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(Error::new(format!("{name} panicked"))),
    }
}

pub fn optimize_json(input: &[u8]) -> Result<Vec<u8>, Error> {
    run_safely("optimize_json", || {
        let v: serde_json::Value = serde_json::from_slice(input)?;
        Ok(serde_json::to_vec(&v)?)
    })
}

pub fn optimize_css(input: &[u8]) -> Result<Vec<u8>, Error> {
    use lightningcss::printer::PrinterOptions;
    use lightningcss::stylesheet::{MinifyOptions, ParserOptions, StyleSheet};

    run_safely("optimize_css", || {
        let s = std::str::from_utf8(input)?;
        let mut sheet = StyleSheet::parse(s, ParserOptions::default())
            .map_err(|e| Error::new(format!("css parse: {e:?}")))?;
        sheet
            .minify(MinifyOptions::default())
            .map_err(|e| Error::new(format!("css minify: {e:?}")))?;
        let result = sheet
            .to_css(PrinterOptions {
                minify: true,
                ..Default::default()
            })
            .map_err(|e| Error::new(format!("css print: {e:?}")))?;
        Ok(result.code.into_bytes())
    })
}

pub fn optimize_html(input: &[u8]) -> Result<Vec<u8>, Error> {
    run_safely("optimize_html", || {
        let cfg = minify_html::Cfg {
            minify_css: true,
            minify_js: true,
            ..Default::default()
        };
        Ok(minify_html::minify(input, &cfg))
    })
}

pub fn optimize_js(input: &[u8]) -> Result<Vec<u8>, Error> {
    use oxc_allocator::Allocator;
    use oxc_codegen::{Codegen, CodegenOptions};
    use oxc_minifier::{Minifier, MinifierOptions};
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    run_safely("optimize_js", || {
        let allocator = Allocator::default();
        let source = std::str::from_utf8(input)?;
        //unambiguous handles both .js (script) and .mjs (module) sources without
        //pre-classification — let the parser figure it out from the source itself
        let parsed = Parser::new(&allocator, source, SourceType::unambiguous()).parse();
        if !parsed.errors.is_empty() {
            return Err(Error::new(format!(
                "js parse: {} error(s)",
                parsed.errors.len()
            )));
        }
        let mut program = parsed.program;
        let _ = Minifier::new(MinifierOptions::default()).minify(&allocator, &mut program);
        let result = Codegen::new()
            .with_options(CodegenOptions {
                minify: true,
                ..CodegenOptions::default()
            })
            .build(&program);
        Ok(result.code.into_bytes())
    })
}

/// Universal size-guard: always Ok; if optimization errored or output ≥ input,
/// return the input bytes verbatim. Callers can feed the result straight into a
/// MemoryTransfer regardless and the bytes will be correct.
pub fn dispatch_by_ext(input: &[u8], ext: &str) -> Result<Vec<u8>, Error> {
    let result = match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => optimize_html(input),
        "css" => optimize_css(input),
        "js" | "mjs" => optimize_js(input),
        "json" => optimize_json(input),
        _ => return Ok(input.to_vec()),
    };
    match result {
        Ok(out) if out.len() < input.len() => Ok(out),
        _ => Ok(input.to_vec()),
    }
}

pub fn applies_to_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "html" | "htm" | "css" | "js" | "mjs" | "json"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strips_whitespace() {
        let input = br#"{
          "a": 1,
          "b": [1, 2, 3]
        }"#;
        let out = dispatch_by_ext(input, "json").unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"{"a":1,"b":[1,2,3]}"#);
    }

    #[test]
    fn json_preserves_key_order() {
        //Regression: without `serde_json/preserve_order`, Value defaults to
        //BTreeMap and silently sorts keys alphabetically — the bytes change
        //in ways that break JWS / canonical-JSON / content-hash consumers.
        //We need source order out the other side.
        let input = br#"{"z":1,"a":2,"m":3}"#;
        let out = dispatch_by_ext(input, "json").unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#"{"z":1,"a":2,"m":3}"#);
    }

    #[test]
    fn json_invalid_falls_through() {
        let input = b"not json";
        assert_eq!(dispatch_by_ext(input, "json").unwrap(), input);
    }

    #[test]
    fn css_minifies() {
        let input = b".foo { color:  red ; padding: 0px;  }";
        let out = dispatch_by_ext(input, "css").unwrap();
        assert!(out.len() < input.len());
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("red"));
    }

    #[test]
    fn html_minifies() {
        let input = b"<html>\n  <body>\n    <p>hi</p>\n  </body>\n</html>";
        let out = dispatch_by_ext(input, "html").unwrap();
        assert!(out.len() < input.len());
    }

    #[test]
    fn js_minifies() {
        let input = b"const longName = 1 + 2;\nconst other = longName + 3;\nconsole.log(other);\n";
        let out = dispatch_by_ext(input, "js").unwrap();
        assert!(out.len() < input.len());
    }

    #[test]
    fn unknown_ext_returns_input_unchanged() {
        let input = b"\x00binary\x01";
        assert_eq!(dispatch_by_ext(input, "bin").unwrap(), input);
    }

    #[test]
    fn svg_falls_through_unoptimized() {
        //SVG isn't optimized in this VMOD (oxvg/lightningcss feature conflict);
        //dispatch should leave it untouched
        let input = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        assert_eq!(dispatch_by_ext(input, "svg").unwrap(), input);
    }

    #[test]
    fn output_never_larger_than_input() {
        //tiny inputs that some optimizers might bloat (e.g. by adding
        //source-map noise) — the guard must still return the original
        let cases: &[(&[u8], &str)] = &[
            (b"<html><body/></html>", "html"),
            (b".a{color:#abc}", "css"),
            (b"x", "js"),
            (b"{}", "json"),
        ];
        for (input, ext) in cases {
            let out = dispatch_by_ext(input, ext).unwrap();
            assert!(
                out.len() <= input.len(),
                "ext={} grew from {} to {} bytes",
                ext,
                input.len(),
                out.len(),
            );
        }
    }

    #[test]
    fn applies_to_ext_includes_supported() {
        for ext in ["html", "htm", "HTML", "css", "CSS", "js", "mjs", "json"] {
            assert!(applies_to_ext(ext), "expected applies for {ext}");
        }
        for ext in ["png", "txt", "svg", "woff2", ""] {
            assert!(!applies_to_ext(ext), "expected NOT applies for {ext}");
        }
    }

    #[test]
    fn run_safely_catches_panic() {
        //Critical FFI invariant: the varnish vmod macro doesn't catch_unwind,
        //so any panic in an optimizer would abort varnishd. run_safely converts
        //panics to ordinary Err values so dispatch_by_ext falls through to the
        //input bytes the same way it does for any parse error.
        let r: Result<Vec<u8>, Error> = run_safely("test", || {
            panic!("boom");
        });
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("panicked"));
    }

    #[test]
    fn run_safely_passes_through_ok() {
        let r = run_safely("test", || Ok(b"hello".to_vec())).unwrap();
        assert_eq!(r, b"hello");
    }

    #[test]
    fn run_safely_passes_through_err() {
        let r: Result<Vec<u8>, Error> = run_safely("test", || Err(Error::new("nope")));
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().to_string(), "nope");
    }
}
