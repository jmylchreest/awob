//! Bounded response framing and deadlines for already-connected Unix streams.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::{Error, Result};

pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
// History retains up to 4 MiB of source/event metadata. JSON escaping can
// expand strings sixfold; keep room for the surrounding Query structure.
pub(super) const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

fn timeout_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "IPC request deadline exceeded",
    )
}

fn remaining(deadline: Instant) -> std::io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(timeout_error)
}

fn normalize_timeout(error: std::io::Error) -> std::io::Error {
    if matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ) {
        timeout_error()
    } else {
        error
    }
}

pub(super) struct DeadlineWriter<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}

impl<'a> DeadlineWriter<'a> {
    pub(super) const fn new(stream: &'a mut UnixStream, deadline: Instant) -> Self {
        Self { stream, deadline }
    }
}

impl Write for DeadlineWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining(self.deadline)?))?;
        self.stream.write(bytes).map_err(normalize_timeout)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        remaining(self.deadline)?;
        self.stream.flush().map_err(normalize_timeout)
    }
}

pub(super) fn read_response_line(
    reader: &mut BufReader<UnixStream>,
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        reader
            .get_ref()
            .set_read_timeout(Some(remaining(deadline)?))?;
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(normalize_timeout(error).into()),
        };
        if available.is_empty() {
            return Ok(line);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |position| position + 1);
        if count > limit - line.len() {
            return Err(Error::ResponseTooLarge { limit });
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(line);
        }
    }
}
