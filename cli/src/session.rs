use jio_client::{Session, VmClient};
use std::env;
use std::ffi::OsString;
use std::io;

pub fn create(host: String) -> io::Result<Session> {
    Ok(client(host)?.create()?.session())
}

pub fn connect(host: String, id: &str) -> io::Result<()> {
    client(host)?.attach(id)?.connect()
}

pub fn destroy(host: String, id: &str) -> io::Result<()> {
    client(host)?.destroy(id)
}

fn client(host: String) -> io::Result<VmClient> {
    let api_key = env::var("JIO_API_KEY")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "JIO_API_KEY is required"))?;
    VmClient::new(host, api_key)
}

pub fn host(value: Option<OsString>) -> io::Result<String> {
    value
        .map(|value| {
            value
                .into_string()
                .map_err(|_| invalid("host is not UTF-8"))
        })
        .transpose()?
        .or_else(|| env::var("JIO_HOST").ok())
        .ok_or_else(|| invalid("missing --host <host> (or JIO_HOST)"))
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
