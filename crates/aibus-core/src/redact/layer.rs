//! The `tracing` side of the redactor: a `fmt` layer whose writer redacts what it is handed.
//! `fmt` formats a whole event into one buffer and writes it once, so a secret is never split
//! across two writes.

use std::io;

use tracing::Subscriber;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::Redactor;

/// A writer that redacts everything before it reaches `inner`.
struct RedactWriter<W> {
    inner: W,
    redactor: Redactor,
}

impl<W: io::Write> io::Write for RedactWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.inner.write_all(self.redactor.redact(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct RedactMakeWriter<M> {
    inner: M,
    redactor: Redactor,
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactMakeWriter<M> {
    type Writer = RedactWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactWriter {
            inner: self.inner.make_writer(),
            redactor: self.redactor.clone(),
        }
    }
}

/// A `tracing` layer that writes events to `writer` with secrets removed. Nothing in the
/// layer's output is coloured.
pub fn layer<S, M>(redactor: Redactor, writer: M) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    M: for<'a> MakeWriter<'a> + 'static,
{
    tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(RedactMakeWriter {
            inner: writer,
            redactor,
        })
}
