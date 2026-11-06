//! An in-process fake adapter for tests, scripted per test to behave like a real one or to
//! misbehave on purpose. Available to other crates with the `test-support` feature.

use crate::client::{DebugAdapter, Incoming};
use crate::transport::{read_message, write_message};
use futures::StreamExt as _;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// What a fake's handler can do in answer to a request.
pub struct Out {
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    pub seq: i64,
    pub request: Value,
    answered: bool,
}

impl Out {
    pub fn send(&mut self, message: Value) {
        let mut writer = self.writer.lock().unwrap();
        if let Some(pipe) = writer.as_mut() {
            write_message(pipe, &message).ok();
        }
    }

    pub fn ok(&mut self, body: Value) {
        let (seq, command) = (self.request["seq"].clone(), self.request["command"].clone());
        self.send(json!({
            "seq": 0, "type": "response", "request_seq": seq,
            "success": true, "command": command, "body": body,
        }));
        self.answered = true;
    }

    pub fn fail(&mut self, body: Value) {
        let (seq, command) = (self.request["seq"].clone(), self.request["command"].clone());
        self.send(json!({
            "seq": 0, "type": "response", "request_seq": seq,
            "success": false, "command": command, "message": "failed", "body": body,
        }));
        self.answered = true;
    }

    /// Answers an earlier request, which the handler held on to.
    pub fn answer(&mut self, seq: i64, command: &str) {
        self.send(json!({
            "seq": 0, "type": "response", "request_seq": seq,
            "success": true, "command": command,
        }));
    }

    pub fn event(&mut self, event: &str, body: Value) {
        self.send(json!({ "seq": 0, "type": "event", "event": event, "body": body }));
    }

    /// Leaves the request unanswered for now.
    pub fn later(&mut self) {
        self.answered = true;
    }

    /// Closes the connection, as a crash would.
    pub fn die(&mut self) {
        self.writer.lock().unwrap().take();
        self.answered = true;
    }

    pub fn command(&self) -> &str {
        self.request["command"].as_str().unwrap_or("")
    }

    pub fn args(&self) -> &Value {
        &self.request["arguments"]
    }
}

pub struct Fake {
    pub adapter: Arc<DebugAdapter>,
    pub incoming: futures::channel::mpsc::UnboundedReceiver<Incoming>,
    /// Everything the client sent, as the fake saw it.
    pub received: mpsc::Receiver<Value>,
}

impl Fake {
    /// The commands the fake was sent so far, in order.
    pub fn commands(&self) -> Vec<String> {
        self.received
            .try_iter()
            .filter(|m| m["type"] == "request")
            .map(|m| m["command"].as_str().unwrap_or("").to_string())
            .collect()
    }

    pub fn next_incoming(&mut self) -> Incoming {
        let next = block_on(async {
            futures::select! {
                next = self.incoming.next() => next,
                _ = futures::FutureExt::fuse(timeout(Duration::from_secs(5))) => None,
            }
        });
        next.expect("something from the adapter")
    }

    /// Events until one called `name`, which is returned.
    pub fn event(&mut self, name: &str) -> Value {
        loop {
            if let Incoming::Event { event, body } = self.next_incoming()
                && event == name
            {
                return body;
            }
        }
    }
}

async fn timeout(duration: Duration) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(duration);
        tx.send(()).ok();
    });
    rx.await.ok();
}

/// A fake answering each request with `handler`. A request it leaves unanswered gets an empty
/// success, unless it called `later`.
pub fn fake(handler: impl FnMut(&mut Out) + Send + 'static) -> Fake {
    let (client_read, server_write) = std::io::pipe().unwrap();
    let (server_read, client_write) = std::io::pipe().unwrap();
    let (received_tx, received) = mpsc::channel();
    serve(server_read, Box::new(server_write), handler, received_tx);
    let (adapter, incoming) = DebugAdapter::from_streams("fake", client_read, client_write);
    Fake {
        adapter,
        incoming,
        received,
    }
}

/// A fake listening on TCP, like js-debug: each connection is a session, answered by a handler
/// made for it from the connection's index. Everything any connection sent arrives in order on
/// the returned receiver, tagged with its connection.
pub fn fake_server<H: FnMut(&mut Out) + Send + 'static>(
    mut handler_for: impl FnMut(usize) -> H + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<(usize, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let Ok(stream) = stream else { return };
            let reader = stream.try_clone().unwrap();
            let (tagged_tx, tagged_rx) = mpsc::channel::<Value>();
            let tx = tx.clone();
            std::thread::spawn(move || {
                for message in tagged_rx {
                    if tx.send((index, message)).is_err() {
                        return;
                    }
                }
            });
            serve(reader, Box::new(stream), handler_for(index), tagged_tx);
        }
    });
    (address, rx)
}

fn serve(
    reader: impl Read + Send + 'static,
    writer: Box<dyn Write + Send>,
    mut handler: impl FnMut(&mut Out) + Send + 'static,
    received: mpsc::Sender<Value>,
) {
    let writer = Arc::new(Mutex::new(Some(writer)));
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        while let Ok(Some(message)) = read_message(&mut reader) {
            received.send(message.clone()).ok();
            if message["type"] != "request" {
                continue;
            }
            let mut out = Out {
                writer: writer.clone(),
                seq: message["seq"].as_i64().unwrap_or(0),
                request: message,
                answered: false,
            };
            handler(&mut out);
            if !out.answered {
                out.ok(Value::Null);
            }
            if writer.lock().unwrap().is_none() {
                return;
            }
        }
    });
}
