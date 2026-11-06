//! DAP's framing, the same as LSP's: a `Content-Length` header, a blank line, then that many bytes
//! of JSON.

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use std::io::{BufRead, Write};

/// The largest message we'll accept. An adapter sending more has gone wrong.
const MAX_MESSAGE: usize = 256 * 1024 * 1024;

/// Reads one message. `Ok(None)` means the stream ended cleanly between messages.
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return if length.is_none() {
                Ok(None)
            } else {
                bail!("the stream ended inside a header")
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse().context("a bad Content-Length")?);
        }
    }
    let length = length.expect("checked above");
    if length > MAX_MESSAGE {
        bail!("a message of {length} bytes is too large");
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .context("the stream ended inside a message")?;
    Ok(Some(
        serde_json::from_slice(&body).context("a message that isn't JSON")?,
    ))
}

pub fn write_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}
