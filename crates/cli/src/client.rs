//! Talks to the daemon over its Unix socket: write one JSON line, read one JSON line back.

use anyhow::{Context, Result, bail};
use philharmonic_core::{Request, Response, paths};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub async fn send(request: &Request) -> Result<Response> {
    let socket = paths::socket_path()?;
    let stream = UnixStream::connect(&socket).await.with_context(|| {
        format!(
            "can't reach the daemon at {}. Is it running? Start it with: cargo run -p philharmonicd",
            socket.display()
        )
    })?;

    let (reader, mut writer) = stream.into_split();

    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;

    let mut reply = String::new();
    let bytes = BufReader::new(reader).read_line(&mut reply).await?;
    if bytes == 0 {
        // The daemon hung up without answering. During development this usually means a
        // `todo!()` or `unwrap()` panicked in the daemon: check its terminal.
        bail!("the daemon closed the connection without answering (did it panic? check its log)");
    }

    serde_json::from_str(&reply).context("the daemon sent something that isn't a valid response")
}
