//! Parsing with one thread-local `tree_sitter::Parser` per worker and a hard deadline.

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::{ParseOptions, ParseState, Parser, Point, Tree};

use crate::grammar::Grammar;
use crate::SyntaxError;

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

/// Parse `source` with `grammar`, cancelling after the `syntax.parse_timeout_ms` setting.
pub(crate) fn parse(grammar: &Grammar, path: &str, source: &[u8]) -> Result<Tree, SyntaxError> {
    PARSER.with(|cell| {
        let mut parser = cell.borrow_mut();
        parser.set_language(&grammar.ts).map_err(|e| SyntaxError::Grammar {
            language: grammar.language,
            message: e.to_string(),
        })?;
        // A previously cancelled parse would otherwise resume.
        parser.reset();

        let started = Instant::now();
        let ms = trace_core::config::current().syntax.parse_timeout_ms;
        let deadline = Duration::from_millis(ms);
        let timed_out = Cell::new(false);
        let mut progress = |_: &ParseState| {
            if started.elapsed() >= deadline {
                timed_out.set(true);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut input = |byte: usize, _: Point| source.get(byte..).unwrap_or(&[]);
        let options = ParseOptions::new().progress_callback(&mut progress);
        let tree = parser.parse_with_options(&mut input, None, Some(options));
        match tree {
            Some(tree) => Ok(tree),
            None => {
                parser.reset();
                if timed_out.get() {
                    Err(SyntaxError::Timeout {
                        path: path.to_string(),
                        ms,
                    })
                } else {
                    Err(SyntaxError::ParseFailed {
                        path: path.to_string(),
                    })
                }
            }
        }
    })
}
