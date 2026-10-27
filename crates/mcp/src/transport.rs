use crate::{Backend, MAX_FRAME_BYTES, Server, error};
use serde_json::Value;
use std::io::{self, BufRead, Write};

/// Stop on an oversized frame instead of buffering or draining unbounded input.
/// EOF between frames is clean shutdown; EOF within a frame is rejected.
pub fn serve(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    backend: &mut impl Backend,
) -> io::Result<()> {
    let mut server = Server::default();
    loop {
        let mut frame = Vec::new();
        let mut limited = io::Read::take(&mut *reader, (MAX_FRAME_BYTES + 2) as u64);
        let count = limited.read_until(b'\n', &mut frame)?;
        if count == 0 {
            return Ok(());
        }
        if frame.last() != Some(&b'\n') || frame.len() > MAX_FRAME_BYTES + 1 {
            let message = if frame.len() > MAX_FRAME_BYTES {
                "Frame exceeds size limit"
            } else {
                "Unterminated frame"
            };
            write_response(writer, &error(Value::Null, -32600, message))?;
            return Err(io::Error::new(io::ErrorKind::InvalidData, message));
        }
        frame.pop();
        if let Some(response) = server.handle(&frame, backend) {
            write_response(writer, &response)?;
        }
    }
}

fn write_response(writer: &mut impl Write, response: &Value) -> io::Result<()> {
    let encoded = serde_json::to_vec(response)?;
    if encoded.len() > MAX_FRAME_BYTES {
        let replacement = error(
            response["id"].clone(),
            -32603,
            "Response exceeds size limit",
        );
        let replacement = serde_json::to_vec(&replacement)?;
        if replacement.len() > MAX_FRAME_BYTES {
            // An oversized echoed ID cannot be correlated within the frame limit.
            serde_json::to_writer(
                &mut *writer,
                &error(Value::Null, -32603, "Response ID exceeds transport limit"),
            )?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Response ID exceeds transport limit",
            ));
        }
        writer.write_all(&replacement)?;
    } else {
        writer.write_all(&encoded)?;
    }
    writer.write_all(b"\n")?;
    writer.flush()
}

/// All stdout writes are protocol frames. Transport diagnostics go to stderr.
pub fn serve_stdio(backend: &mut impl Backend) -> io::Result<()> {
    let result = serve(&mut io::stdin().lock(), &mut io::stdout().lock(), backend);
    if let Err(error) = &result {
        eprintln!("MCP transport failed: {error}");
    }
    result
}
