//! Pure Server-Sent Events frame reader.
//!
//! The hub's `/api/v1/events` stream is consumed only for its change signal:
//! the name of each dispatched frame is enough to know the hub has new state,
//! so frame payloads are deliberately ignored. This module has no HTTP or
//! runtime dependency, which keeps the parsing rules unit-testable over any
//! [`std::io::BufRead`].

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};

/// Read SSE frames from `reader` until EOF, a read error, or `stop` is set.
///
/// `on_frame` is called with the `event:` name of each dispatched frame. A
/// frame is dispatched on a blank line; `:`-prefixed lines are comments (the
/// hub's keep-alive) and are ignored. Per the SSE spec, a frame with no
/// `event:` field dispatches the empty name, and multi-line `data:` fields
/// still produce a single dispatch. An incomplete frame at EOF is discarded.
pub(crate) fn read_frames<R: BufRead>(
    mut reader: R,
    stop: &AtomicBool,
    on_frame: &mut dyn FnMut(&str),
) {
    let mut event_name = String::new();
    let mut line = String::new();
    let mut in_frame = false;

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }

        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            if in_frame {
                on_frame(&event_name);
            }
            event_name.clear();
            in_frame = false;
            continue;
        }

        if trimmed.starts_with(':') {
            continue;
        }

        in_frame = true;
        let (field, value) = match trimmed.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (trimmed, ""),
        };
        if field == "event" {
            event_name.clear();
            event_name.push_str(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    use super::read_frames;

    fn frames(input: &str) -> Vec<String> {
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        read_frames(Cursor::new(input.as_bytes()), &stop, &mut |name| {
            seen.push(name.to_string());
        });
        seen
    }

    #[test]
    fn dispatches_a_single_update_frame() {
        assert_eq!(
            frames("event: update\ndata: {\"revision\":1}\n\n"),
            ["update"]
        );
    }

    #[test]
    fn ignores_keep_alive_comments() {
        assert_eq!(
            frames(": keep-alive\n\n: keep-alive\n\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn coalesces_multi_line_data_into_one_frame() {
        assert_eq!(
            frames("event: update\ndata: line one\ndata: line two\n\n"),
            ["update"]
        );
    }

    #[test]
    fn dispatches_a_frame_with_no_data() {
        assert_eq!(frames("event: lagged\n\n"), ["lagged"]);
    }

    #[test]
    fn returns_at_eof_without_dispatching_a_partial_frame() {
        assert_eq!(frames("event: update\ndata: {}"), Vec::<String>::new());
    }
}
