//! Line-oriented redaction for `tracing` JSON output.
//!
//! [`super::init`] attaches two `tracing_subscriber::fmt` JSON layers: one to stdout and one to
//! the daily log file. Before this module existed that path never called
//! [`super::logs::redact_sensitive_log_values`], so an event whose field carried a complete
//! external URL, `token=...`, or `Authorization: ...` reached both sinks verbatim even though
//! `docs/LUX-DEVELOPMENT.md` §18.3 forbids it. [`RedactingMakeWriter`] wraps a `MakeWriter` so
//! every complete JSON line goes through the same redaction the application-event path uses.
//!
//! The wrapper is deliberately conservative about when it does work:
//!
//! - A cheap byte scan ([`may_need_redaction`]) decides whether a line could hold anything the
//!   redactor would rewrite. Lines that cannot are written as their original bytes, which keeps
//!   field order and formatting and keeps the hot path free of JSON parsing. A whole event that
//!   arrives as one terminated line is also processed in place, without copying it into the
//!   buffer first.
//! - A line is only re-serialised when redaction actually changed the parsed value; parse and
//!   serialisation failures fall back to the original bytes so a log line is never dropped.
//! - When the wrapped writer itself fails, the lines that already reached the sink are removed
//!   from the buffer before the error propagates, so the retry performed by `flush`/`drop` cannot
//!   emit them twice. The failed line and everything after it stay buffered, so that retry can
//!   still deliver them.
//! - Text without a trailing newline is buffered and written on `flush`/`drop`; the writer never
//!   swallows a partial line. Such a tail is passed through the same redaction, so a complete
//!   JSON document without its newline is still redacted while a truncated one is copied
//!   verbatim.
//!
//! # Field order
//!
//! A rewritten line is re-serialised from a [`serde_json::Value`], and this crate builds
//! `serde_json` without its `preserve_order` feature: **object keys inside a redacted line are
//! emitted in sorted order**, so the first field of such a line is no longer necessarily
//! `timestamp`. Lines that need no change are written as their original bytes, so their field
//! order and formatting are byte-for-byte identical to what the layer produced. Consumers must
//! therefore read `tracing` output by key (`value["fields"]["message"]`, `value["span"]["path"]`)
//! instead of depending on field positions or on the leading field of a line. Enabling
//! `preserve_order` would keep the order but adds a dependency feature to both sinks, so the
//! sorted-key behaviour is the accepted trade-off.

use std::io::{self, Write};

use serde_json::Value;
use tracing_subscriber::fmt::MakeWriter;

use super::logs::redact_sensitive_log_values;

/// Wraps a [`MakeWriter`] so every line it emits is redacted before it is written.
pub(crate) struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    pub(crate) fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<'a, M> MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(self.inner.make_writer())
    }
}

/// Buffers one line at a time and writes it through [`redact_sensitive_log_values`].
pub(crate) struct RedactingWriter<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            pending: Vec::new(),
        }
    }

    /// Writes every buffered line that already ended, keeping the unterminated tail buffered.
    fn write_complete_lines(&mut self) -> io::Result<()> {
        let mut consumed = 0;
        while let Some(offset) = self.pending[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = consumed + offset + 1;
            if let Err(error) = write_redacted_line(&mut self.inner, &self.pending[consumed..end]) {
                // The lines before `consumed` already reached the sink, so drop them before
                // unwinding: a later `flush`/`drop` retry must never emit them twice. The failed
                // line and everything after it stay buffered so a retry can still deliver them;
                // keeping the failed line is the lesser evil because dropping it would silently
                // lose a log line, and `write_all` cannot tell us how much of it was written.
                self.pending.drain(..consumed);
                return Err(error);
            }
            consumed = end;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    /// Writes the unterminated tail as-is and flushes the wrapped writer.
    fn flush_pending(&mut self) -> io::Result<()> {
        // A line buffered by an earlier failed write is retried here per line, so the retry path
        // keeps the same redaction as the initial one.
        self.write_complete_lines()?;
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            write_redacted_line(&mut self.inner, &pending)?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        // The `fmt` layer hands over a whole event in one call, so the common case is a single
        // terminated line: process it in place instead of copying it into the buffer.
        if self.pending.is_empty() && is_single_terminated_line(bytes) {
            if let Err(error) = write_redacted_line(&mut self.inner, bytes) {
                // Keep the event for the retry in `flush`/`drop`, exactly like the buffered path
                // below; this is the only error path that has to copy.
                self.pending.extend_from_slice(bytes);
                return Err(error);
            }
            return Ok(bytes.len());
        }
        self.pending.extend_from_slice(bytes);
        self.write_complete_lines()?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()
    }
}

fn is_single_terminated_line(bytes: &[u8]) -> bool {
    match bytes.split_last() {
        Some((b'\n', body)) => !body.contains(&b'\n'),
        _ => false,
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        // A formatter that ends without a newline still gets its bytes; errors cannot be
        // reported from `drop`.
        let _ = self.flush_pending();
    }
}

/// Fragments the value rules search for inside a lowercased string. A value that the redactor
/// rewrites contains one of them contiguously, so a case-insensitive substring test is exact for
/// this half of the rules.
const VALUE_FRAGMENTS: [&[u8]; 9] = [
    b"password",
    b"token",
    b"secret",
    b"cookie",
    b"authorization",
    b"apikey",
    b"api_key",
    b"bearer",
    b"http",
];

/// Fragments the key rules look for once non-alphanumeric separators are stripped. Keys are
/// contiguous in the raw line, so any key that contains one of them also contains its letters in
/// order; a subsequence scan of the key token is therefore a strict superset of the key rule.
const KEY_FRAGMENTS: [&[u8]; 7] = [
    b"password",
    b"token",
    b"secret",
    b"cookie",
    b"authorization",
    b"apikey",
    b"credential",
];

/// Cheap, conservative pre-check: `false` only when the redactor provably cannot change the line.
fn may_need_redaction(line: &[u8]) -> bool {
    // JSON escapes hide characters (a key written as `"\u0074oken"` parses to `token`), so any
    // escape sequence forces the slow path.
    if line
        .windows(2)
        .any(|window| window[0] == b'\\' && window[1] == b'u')
    {
        return true;
    }
    VALUE_FRAGMENTS
        .iter()
        .any(|fragment| contains_ignore_case(line, fragment))
        || key_fragments_present(line)
}

fn contains_ignore_case(line: &[u8], fragment: &[u8]) -> bool {
    if fragment.is_empty() || line.len() < fragment.len() {
        return false;
    }
    line.windows(fragment.len())
        .any(|window| window.eq_ignore_ascii_case(fragment))
}

/// Scans the JSON key tokens for a fragment the key rules would rewrite.
fn key_fragments_present(line: &[u8]) -> bool {
    let mut index = 0;
    while index < line.len() {
        if line[index] != b'"' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut cursor = start;
        let mut escaped = false;
        while cursor < line.len() {
            let byte = line[cursor];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                break;
            }
            cursor += 1;
        }
        if cursor >= line.len() {
            return false;
        }
        if terminates_as_key(line, cursor + 1) && key_is_sensitive(&line[start..cursor]) {
            return true;
        }
        index = cursor + 1;
    }
    false
}

/// Whether the next non-whitespace byte after a closing quote is `:`; only key tokens are
/// followed by a colon in JSON.
fn terminates_as_key(line: &[u8], mut index: usize) -> bool {
    while index < line.len() {
        match line[index] {
            b' ' | b'\t' | b'\r' => index += 1,
            b':' => return true,
            _ => return false,
        }
    }
    false
}

fn key_is_sensitive(key: &[u8]) -> bool {
    KEY_FRAGMENTS
        .iter()
        .any(|fragment| contains_normalized_subsequence(key, fragment))
        || normalized_suffix_is_url_or_uri(key)
}

/// Whether `fragment` appears in `text` when non-alphanumeric separators are ignored.
///
/// The scan is sequential, so it also accepts letters spread across a longer run of text; that
/// only costs a JSON parse, never a missed redaction.
fn contains_normalized_subsequence(text: &[u8], fragment: &[u8]) -> bool {
    if fragment.is_empty() {
        return false;
    }
    let mut matched = 0;
    for &byte in text {
        if !byte.is_ascii_alphanumeric() {
            continue;
        }
        if byte.to_ascii_lowercase() == fragment[matched] {
            matched += 1;
            if matched == fragment.len() {
                return true;
            }
        }
    }
    false
}

/// Exact test for the `url`/`uri` key suffix rule, which looks at the key's trailing letters.
fn normalized_suffix_is_url_or_uri(key: &[u8]) -> bool {
    let mut tail = [0_u8; 3];
    let mut count = 0;
    for &byte in key.iter().rev() {
        if !byte.is_ascii_alphanumeric() {
            continue;
        }
        if count == tail.len() {
            break;
        }
        tail[count] = byte.to_ascii_lowercase();
        count += 1;
    }
    if count < tail.len() {
        return false;
    }
    // `tail` is filled from the end: index 0 is the last character, index 2 the third-to-last.
    matches!(
        (tail[0], tail[1], tail[2]),
        (b'l', b'r', b'u') | (b'i', b'r', b'u')
    )
}

/// Redacted encoding of `line`, or `None` when the line is safe or could not be processed.
fn redact_line(line: &[u8]) -> Option<Vec<u8>> {
    if !may_need_redaction(line) {
        return None;
    }
    let (body, terminated) = match line.strip_suffix(b"\n") {
        Some(body) => (body, true),
        None => (line, false),
    };
    let original = serde_json::from_slice::<Value>(body).ok()?;
    let redacted = redact_sensitive_log_values(original.clone());
    if redacted == original {
        return None;
    }
    let mut encoded = serde_json::to_vec(&redacted).ok()?;
    if terminated {
        encoded.push(b'\n');
    }
    Some(encoded)
}

fn write_redacted_line<W: Write>(inner: &mut W, line: &[u8]) -> io::Result<()> {
    match redact_line(line) {
        Some(redacted) => inner.write_all(&redacted),
        None => inner.write_all(line),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fmt as std_fmt,
        io::{self, Write},
        sync::{Arc, Mutex, MutexGuard, PoisonError},
    };

    use serde_json::Value;
    use tracing_subscriber::{
        fmt as tracing_fmt,
        fmt::{MakeWriter, time::FormatTime},
        layer::SubscriberExt,
    };

    use super::{RedactingMakeWriter, RedactingWriter, may_need_redaction};

    /// In-memory sink shared by the test subscriber and the assertions.
    #[derive(Clone, Default)]
    struct SharedBuffer {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBuffer {
        fn captured(&self) -> Vec<u8> {
            lock(&self.bytes).clone()
        }

        fn captured_text(&self) -> String {
            String::from_utf8_lossy(&self.captured()).into_owned()
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    impl Write for SharedBuffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            lock(&self.bytes).extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedBuffer {
        type Writer = SharedBuffer;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Keeps both subscribers byte-comparable by removing wall-clock time from the output.
    struct FixedTimer;

    impl FormatTime for FixedTimer {
        fn format_time(&self, writer: &mut tracing_fmt::format::Writer<'_>) -> std_fmt::Result {
            writer.write_str("2026-01-01T00:00:00.000000Z")
        }
    }

    /// Runs `events` under the same JSON layer shape `init` installs, but the writer is wrapped.
    fn emit_redacted<F: FnOnce()>(buffer: &SharedBuffer, events: F) {
        let subscriber = tracing_subscriber::registry().with(
            tracing_fmt::layer()
                .json()
                .with_ansi(false)
                .with_timer(FixedTimer)
                .with_writer(RedactingMakeWriter::new(buffer.clone())),
        );
        tracing::subscriber::with_default(subscriber, events);
    }

    /// Same layer shape without the wrapper: the reference bytes for "nothing changed".
    fn emit_unwrapped<F: FnOnce()>(buffer: &SharedBuffer, events: F) {
        let subscriber = tracing_subscriber::registry().with(
            tracing_fmt::layer()
                .json()
                .with_ansi(false)
                .with_timer(FixedTimer)
                .with_writer(buffer.clone()),
        );
        tracing::subscriber::with_default(subscriber, events);
    }

    fn parse_event(text: &str) -> Value {
        let mut lines = text.lines();
        let line = lines.next().expect("one event line");
        assert!(lines.next().is_none(), "expected a single line: {text}");
        serde_json::from_str(line).expect("tracing JSON event")
    }

    #[test]
    fn error_display_with_a_complete_url_is_redacted() {
        let buffer = SharedBuffer::default();
        emit_redacted(&buffer, || {
            let error = "request to https://example.com/items/a?token=abc failed";
            tracing::warn!(%error, "media request failed");
        });

        let text = buffer.captured_text();
        assert!(!text.contains("example.com"), "{text}");
        assert!(!text.contains("token=abc"), "{text}");
        assert!(!text.contains("https://"), "{text}");

        let event = parse_event(&text);
        assert_eq!(
            event["fields"]["error"],
            Value::String("[REDACTED_URL]".to_owned())
        );
        assert_eq!(
            event["fields"]["message"],
            Value::String("media request failed".to_owned())
        );
    }

    #[test]
    fn credential_fields_are_replaced() {
        let buffer = SharedBuffer::default();
        emit_redacted(&buffer, || {
            tracing::warn!(
                api_key = "sk-live-value",
                authorization = "Bearer abcdef",
                cookie = "session=abcdef",
                password = "hunter2",
                "login rejected"
            );
        });

        let event = parse_event(&buffer.captured_text());
        for field in ["api_key", "authorization", "cookie", "password"] {
            assert_eq!(
                event["fields"][field],
                Value::String("[REDACTED]".to_owned()),
                "{field}"
            );
        }
    }

    /// Request span fields and event fields the specification relies on must survive untouched.
    fn emit_normal_events() {
        let span = tracing::info_span!(
            "request",
            requestId = "req-42",
            durationMs = 17_u64,
            statusCode = 200_u16,
            errorCode = "none",
            path = "/api/movies",
            method = "GET"
        );
        let _entered = span.enter();
        tracing::info!(count = 3_u64, "listed movies");
        tracing::debug!("no fields at all");
    }

    #[test]
    fn events_without_sensitive_values_keep_their_exact_bytes() {
        let wrapped = SharedBuffer::default();
        let plain = SharedBuffer::default();
        emit_redacted(&wrapped, emit_normal_events);
        emit_unwrapped(&plain, emit_normal_events);
        assert_eq!(wrapped.captured(), plain.captured());
    }

    #[test]
    fn reserialization_keeps_structure_and_benign_fields() {
        let buffer = SharedBuffer::default();
        emit_redacted(&buffer, || {
            let span = tracing::info_span!(
                "request",
                requestId = "req-42",
                durationMs = 17_u64,
                statusCode = 200_u16,
                errorCode = "none",
                path = "/api/movies",
                method = "GET"
            );
            let _entered = span.enter();
            tracing::warn!(secret_value = "leaked", "redaction forced");
        });

        let event = parse_event(&buffer.captured_text());
        assert_eq!(
            event["fields"]["secret_value"],
            Value::String("[REDACTED]".to_owned())
        );
        assert_eq!(event["level"], Value::String("WARN".to_owned()));
        assert_eq!(
            event["timestamp"],
            Value::String("2026-01-01T00:00:00.000000Z".to_owned())
        );
        assert!(
            event["target"]
                .as_str()
                .is_some_and(|target| target.ends_with("tests")),
            "{}",
            event["target"]
        );

        let span = &event["span"];
        assert_eq!(span["name"], Value::String("request".to_owned()));
        assert_eq!(span["requestId"], Value::String("req-42".to_owned()));
        assert_eq!(span["durationMs"], Value::from(17_u64));
        assert_eq!(span["statusCode"], Value::from(200_u16));
        assert_eq!(span["errorCode"], Value::String("none".to_owned()));
        assert_eq!(span["path"], Value::String("/api/movies".to_owned()));
        assert_eq!(span["method"], Value::String("GET".to_owned()));
    }

    #[test]
    fn non_json_and_unterminated_tails_are_written_verbatim() {
        let buffer = SharedBuffer::default();
        let mut writer = RedactingWriter::new(buffer.clone());
        writer
            .write_all(b"plain text line\n{\"fields\":{\"token\":\"ab")
            .expect("write");
        writer.flush().expect("flush");

        let expected = b"plain text line\n{\"fields\":{\"token\":\"ab";
        assert_eq!(buffer.captured().as_slice(), expected.as_slice());

        drop(writer);
        assert_eq!(buffer.captured().as_slice(), expected.as_slice());
    }

    #[test]
    fn a_complete_json_tail_without_a_newline_is_still_redacted() {
        let buffer = SharedBuffer::default();
        let mut writer = RedactingWriter::new(buffer.clone());
        writer
            .write_all(b"{\"fields\":{\"token\":\"abc\"}}")
            .expect("write");
        drop(writer);

        let text = buffer.captured_text();
        assert!(!text.contains("abc"), "{text}");
        assert!(serde_json::from_str::<Value>(&text).is_ok(), "{text}");
    }

    /// Inner writer that fails one chosen `write` call and records everything it accepted.
    struct FlakyWriter {
        buffer: SharedBuffer,
        fail_on_call: usize,
        calls: usize,
    }

    impl Write for FlakyWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls == self.fail_on_call {
                return Err(io::Error::other("sink stopped accepting bytes"));
            }
            self.buffer.write_all(bytes)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_write_never_repeats_lines_that_already_reached_the_sink() {
        let buffer = SharedBuffer::default();
        let mut writer = RedactingWriter::new(FlakyWriter {
            buffer: buffer.clone(),
            fail_on_call: 2,
            calls: 0,
        });
        let first = "{\"fields\":{\"message\":\"first\"}}\n";
        let second = "{\"fields\":{\"message\":\"second\"}}\n";
        let third = "{\"fields\":{\"message\":\"third\"}}\n";

        let error = writer
            .write_all(format!("{first}{second}{third}").as_bytes())
            .expect_err("the second inner write fails");
        assert_eq!(error.kind(), io::ErrorKind::Other);

        // The retry (here an explicit flush, then the implicit one in `drop`) still delivers the
        // failed line and the line after it, but must not replay the line that already landed.
        writer.flush().expect("retry succeeds");
        drop(writer);

        let text = buffer.captured_text();
        assert_eq!(text.matches("\"first\"").count(), 1, "{text}");
        assert_eq!(text.matches("\"second\"").count(), 1, "{text}");
        assert_eq!(text.matches("\"third\"").count(), 1, "{text}");
        assert_eq!(text.lines().count(), 3, "{text}");
    }

    #[test]
    fn a_retried_line_keeps_the_redaction_of_the_first_attempt() {
        let buffer = SharedBuffer::default();
        let mut writer = RedactingWriter::new(FlakyWriter {
            buffer: buffer.clone(),
            fail_on_call: 1,
            calls: 0,
        });
        let error = writer
            .write_all(b"{\"fields\":{\"token\":\"tail-secret\"}}\n")
            .expect_err("the only inner write fails");
        assert_eq!(error.kind(), io::ErrorKind::Other);

        drop(writer);
        let text = buffer.captured_text();
        assert!(
            !text.contains("tail-secret"),
            "retry leaked the value: {text}"
        );
        let event: Value = serde_json::from_str(text.trim_end()).expect("JSON event");
        assert_eq!(
            event["fields"]["token"],
            Value::String("[REDACTED]".to_owned())
        );
    }

    #[test]
    fn events_over_64_kib_are_handled_without_panicking() {
        let buffer = SharedBuffer::default();
        let secret = format!("{}?token=abc", "x".repeat(70 * 1024));
        let plain = "y".repeat(70 * 1024);
        emit_redacted(&buffer, || {
            tracing::warn!(payload = %secret, "big secret event");
            tracing::info!(payload = %plain, "big plain event");
        });

        let text = buffer.captured_text();
        assert!(!text.contains("token=abc"), "secret survived");
        let mut lines = text.lines();
        let first = serde_json::from_str::<Value>(lines.next().expect("first line"))
            .expect("first JSON event");
        let second = serde_json::from_str::<Value>(lines.next().expect("second line"))
            .expect("second JSON event");
        assert!(lines.next().is_none());
        assert_eq!(
            first["fields"]["payload"],
            Value::String("[REDACTED]".to_owned())
        );
        assert_eq!(
            second["fields"]["payload"],
            Value::String(plain),
            "a benign long payload must stay intact"
        );
    }

    #[test]
    fn lines_split_across_writes_are_redacted() {
        let buffer = SharedBuffer::default();
        let mut writer = RedactingWriter::new(buffer.clone());
        let secret = format!(
            "{{\"fields\":{{\"token\":\"{}\"}}}}\n",
            "z".repeat(70 * 1024)
        );
        for chunk in secret.as_bytes().chunks(4096) {
            writer.write_all(chunk).expect("write");
        }
        writer.flush().expect("flush");

        let text = buffer.captured_text();
        assert_eq!(
            text.matches('\n').count(),
            1,
            "line was split or duplicated"
        );
        assert!(!text.contains("zzzz"), "payload survived");
        let event = serde_json::from_str::<Value>(text.trim_end()).expect("JSON event");
        assert_eq!(
            event["fields"]["token"],
            Value::String("[REDACTED]".to_owned())
        );
    }

    #[test]
    fn multiple_lines_in_one_write_are_each_redacted() {
        let buffer = SharedBuffer::default();
        let mut writer = RedactingWriter::new(buffer.clone());
        writer
            .write_all(
                b"{\"fields\":{\"token\":\"one\"}}\n{\"fields\":{\"message\":\"two steps\"}}\n",
            )
            .expect("write");

        let text = buffer.captured_text();
        let mut lines = text.lines();
        let first = serde_json::from_str::<Value>(lines.next().expect("first line"))
            .expect("first JSON event");
        let second = serde_json::from_str::<Value>(lines.next().expect("second line"))
            .expect("second JSON event");
        assert!(lines.next().is_none());
        assert_eq!(
            first["fields"]["token"],
            Value::String("[REDACTED]".to_owned())
        );
        assert_eq!(
            second["fields"]["message"],
            Value::String("two steps".to_owned())
        );
    }

    #[test]
    fn fast_path_covers_separator_variants_of_sensitive_keys() {
        for line in [
            br#"{"fields":{"api-key":"abc"}}"#.as_slice(),
            br#"{"fields":{"api.key":"abc"}}"#.as_slice(),
            br#"{"fields":{"access_token":"abc"}}"#.as_slice(),
            br#"{"fields":{"sourceUrl":"abc"}}"#.as_slice(),
            br#"{"fields":{"pass_word":"abc"}}"#.as_slice(),
            br#"{"fields":{"X-Api-Key":"abc"}}"#.as_slice(),
            br#"{"fields":{"Authorization":"abc"}}"#.as_slice(),
            br#"{"fields":{"credentialId":"abc"}}"#.as_slice(),
            br#"{"fields":{"redirect_uri":"abc"}}"#.as_slice(),
            br#"{"fields":{"note":"https://example.com/x?token=abc"}}"#.as_slice(),
            br#"{"fields":{"note":"authorization: Bearer abc"}}"#.as_slice(),
        ] {
            assert!(
                may_need_redaction(line),
                "fast path missed {}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn fast_path_skips_ordinary_lines() {
        for line in [
            br#"{"timestamp":"2026-01-01T00:00:00Z","level":"INFO","fields":{"message":"server started"},"target":"luxd"}"#.as_slice(),
            br#"{"fields":{"requestId":"req-42","durationMs":17,"statusCode":200,"errorCode":"none","path":"/api/movies","method":"GET"}}"#.as_slice(),
        ] {
            assert!(
                !may_need_redaction(line),
                "fast path parsed {}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn escaped_json_keys_force_the_slow_path() {
        assert!(may_need_redaction(br#"{"fields":{"\u0074oken":"abc"}}"#));
    }

    /// The daily file sink goes through the same wrapper `init` installs for `LogWriter`.
    #[tokio::test]
    async fn file_sink_output_is_redacted() -> io::Result<()> {
        use super::super::logs::{LogStore, log_dir, log_file_name};

        let temp_dir = tempfile::tempdir()?;
        let store = LogStore::open(temp_dir.path()).await?;
        // A closure is a `MakeWriter`; `init` wraps the non-blocking handle the same way.
        let make_writer = {
            let store = store.clone();
            move || store.writer()
        };
        let subscriber = tracing_subscriber::registry().with(
            tracing_fmt::layer()
                .json()
                .with_ansi(false)
                .with_timer(FixedTimer)
                .with_writer(RedactingMakeWriter::new(make_writer)),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                request_url = "https://example.com/x?token=abc",
                "file probe"
            );
        });

        let path =
            log_dir(temp_dir.path()).join(log_file_name(time::OffsetDateTime::now_utc().date()));
        let contents = std::fs::read_to_string(path)?;
        assert!(!contents.contains("example.com"), "{contents}");
        assert!(!contents.contains("token=abc"), "{contents}");
        let event: Value = serde_json::from_str(contents.lines().next().unwrap_or_default())
            .map_err(io::Error::other)?;
        assert_eq!(
            event["fields"]["message"],
            Value::String("file probe".to_owned())
        );
        Ok(())
    }
}
