//! Private local control protocol; no shell commands or model calls from UI refreshes.
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};
pub fn request(home: &Path, request: Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(home.join("control.sock"))
        .map_err(|error| format!("Cannot reach Monohero service: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let response: Value = serde_json::from_str(&line)
        .map_err(|error| format!("Invalid service response: {error}"))?;
    if response["ok"] != true {
        return Err(format!(
            "{}. Queue management requires Monohero 0.4 or newer.",
            response["error"].as_str().unwrap_or("Request failed")
        ));
    }
    Ok(response["data"].clone())
}
