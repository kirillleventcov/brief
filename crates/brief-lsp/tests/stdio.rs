//! End-to-end test over the real stdio transport: the server must survive
//! documents whose diagnostics land on multibyte characters.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

/// Long enough for a debug build on a loaded machine; a server that has
/// crashed or wedged never answers at all.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Kills the server however the test ends, so a failure cannot leak it.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn send(stdin: &mut ChildStdin, body: &str) {
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

/// Read one framed message body.
fn read_message(stdout: &mut BufReader<ChildStdout>) -> Option<String> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            len = n.parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; len?];
    stdout.read_exact(&mut body).ok()?;
    String::from_utf8(body).ok()
}

/// Server messages, read on a thread so a dead or wedged server shows up
/// as a timeout instead of a hung test.
fn messages(stdout: ChildStdout) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        while let Some(msg) = read_message(&mut stdout) {
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
    rx
}

fn next(rx: &Receiver<String>, waiting_for: &str) -> String {
    rx.recv_timeout(TIMEOUT)
        .unwrap_or_else(|_| panic!("server died or hung waiting for {waiting_for}"))
}

/// Skip notifications until the response to request `id`.
fn response(rx: &Receiver<String>, id: u32) -> String {
    let needle = format!("\"id\":{}", id);
    loop {
        let msg = next(rx, &format!("response {id}"));
        if msg.contains(&needle) && !msg.contains("\"method\"") {
            return msg;
        }
    }
}

fn did_open(uri: &str, text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"brief","version":1,"text":"{escaped}"}}}}}}"#
    )
}

#[test]
fn server_survives_multibyte_diagnostics() {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_brief-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn brief-lsp"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let rx = messages(server.0.stdout.take().unwrap());

    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
    );
    response(&rx, 1);
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
    );

    // Untitled buffers are analyzed without a project, so the result does
    // not depend on the filesystem.
    let docs = [
        "@callout(kind: é)\nx\n@end",
        "@callout(kind: 😀)\nx\n@end\n",
        "日本語 *強調\n",
    ];
    for (i, text) in docs.iter().enumerate() {
        send(&mut stdin, &did_open(&format!("untitled:doc{i}"), text));
    }
    let mut published = 0;
    while published < docs.len() {
        if next(&rx, "publishDiagnostics").contains("textDocument/publishDiagnostics") {
            published += 1;
        }
    }

    // A hover on the multibyte character goes through the reverse
    // conversion too.
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":"untitled:doc0"},"position":{"line":0,"character":16}}}"#,
    );
    response(&rx, 2);

    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}"#,
    );
    response(&rx, 3);
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"exit","params":null}"#,
    );
    drop(stdin);
    let status = server.0.wait().unwrap();
    assert!(status.success(), "{:?}", status);
}
