#![allow(dead_code)]
use baidu_drive_studio::{
    api::{Api, Endpoints},
    model::Credentials,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
pub type Requests = Arc<Mutex<Vec<Vec<u8>>>>;
pub fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}
pub fn json(body: &str) -> String {
    response("200 OK", "Content-Type: application/json\r\n", body)
}
pub fn server(
    replies: Vec<String>,
    credentials: Credentials,
) -> (Api, Requests, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests: Requests = Default::default();
    let captured = requests.clone();
    let server_base = base.clone();
    let task = thread::spawn(move || {
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(s) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("expected request missing: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = vec![];
            let mut buffer = [0; 8192];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..n]);
                if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..end]);
                    let size = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            captured.lock().unwrap().push(data);
            let reply = reply.replace("{{BASE}}", &server_base);
            let (head, body) = reply.split_once("\r\n\r\n").unwrap();
            let head = head
                .lines()
                .filter(|l| !l.to_ascii_lowercase().starts_with("content-length:"))
                .collect::<Vec<_>>()
                .join("\r\n");
            let reply = format!("{head}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    let api = Api::with_endpoints(
        credentials,
        Endpoints {
            pan: base.clone(),
            oauth: base.clone(),
            pcs: base,
        },
    )
    .unwrap();
    (api, requests, task)
}
