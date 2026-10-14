//! The window's line to the daemon: the event stream on one connection, each request on a
//! connection of its own (`docs/events.md`). It never starts the daemon.

use anyhow::{Result, bail};
use melodie_core::proto::{self, Event, Request};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often the window looks for a daemon while there is none (like `melodie watch`).
pub const RETRY: Duration = Duration::from_millis(500);
/// Time left to a request: they are all quick, the window never loads tracks itself.
const REPLY_WAIT: Duration = Duration::from_secs(5);

/// What the stream tells the window.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Event(Box<Event>),
    /// No daemon, at start or when the connection ends. Said once until a daemon comes back.
    Gone,
}

/// One line of the stream; `None` for an event type this window does not know (the contract
/// lets the daemon add some).
pub fn decode_event(line: &str) -> Option<Event> {
    proto::decode(line).ok()
}

/// Follows the daemon on `socket` from a thread of its own until `stop` is set, sending what
/// happens to `send`. Ends too when `send` says the window is gone (returns false).
pub fn follow(
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    mut send: impl FnMut(Msg) -> bool + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut said_gone = false;
        while !stop.load(Ordering::Relaxed) {
            if let Ok(stream) = UnixStream::connect(&socket)
                && (&stream).write_all(b"{\"cmd\":\"watch\"}\n").is_ok()
            {
                for line in BufReader::new(&stream).lines() {
                    let Ok(line) = line else { break };
                    if let Some(e) = decode_event(&line) {
                        said_gone = false;
                        if !send(Msg::Event(Box::new(e))) {
                            return;
                        }
                    }
                }
            }
            if !said_gone {
                if !send(Msg::Gone) {
                    return;
                }
                said_gone = true;
            }
            std::thread::sleep(RETRY);
        }
    })
}

/// Sends one request and waits for its answer.
pub fn request(socket: &Path, req: &Request) -> Result<()> {
    let stream = UnixStream::connect(socket)?;
    let r = proto::exchange(&stream, req, REPLY_WAIT)?;
    if !r.ok {
        bail!(
            "{}",
            r.error.unwrap_or_else(|| "refused by the daemon".into())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use melodie_core::proto::{Now, QueueView, Response, encode};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc::{Receiver, channel};

    fn snapshot(title: &str) -> Event {
        Event::Snapshot {
            now: Now {
                state: "playing".into(),
                title: title.into(),
                ..Now::default()
            },
            queue: QueueView::default(),
            connection: "ok".into(),
        }
    }

    /// A daemon that answers `watch` with the given lines, then closes the connection.
    fn serve_once(listener: &UnixListener, lines: &[String]) {
        let (stream, _) = listener.accept().unwrap();
        let mut req = String::new();
        BufReader::new(&stream).read_line(&mut req).unwrap();
        assert_eq!(req.trim(), r#"{"cmd":"watch"}"#);
        let mut w = &stream;
        for l in lines {
            w.write_all(format!("{l}\n").as_bytes()).unwrap();
        }
    }

    fn start(socket: PathBuf) -> (Receiver<Msg>, Arc<AtomicBool>) {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        follow(socket, stop.clone(), move |m| tx.send(m).is_ok());
        (rx, stop)
    }

    fn next(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(5)).expect("a message")
    }

    #[test]
    fn unknown_event_types_and_fields_are_ignored() {
        assert_eq!(decode_event(r#"{"type":"lyrics","text":"la"}"#), None);
        assert_eq!(decode_event("not json"), None);
        assert_eq!(
            decode_event(r#"{"type":"volume","volume":0.5,"later":true}"#),
            Some(Event::Volume { volume: 0.5 })
        );
    }

    #[test]
    fn no_daemon_at_start_says_gone_once() {
        let dir = tempfile::tempdir().unwrap();
        let (rx, stop) = start(dir.path().join("melodie.sock"));
        assert_eq!(next(&rx), Msg::Gone);
        assert!(
            rx.recv_timeout(RETRY * 3).is_err(),
            "gone is said once, not at every retry"
        );
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn the_stream_then_daemon_gone_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("melodie.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (rx, stop) = start(socket.clone());
        serve_once(
            &listener,
            &[
                encode(&snapshot("First")).unwrap(),
                r#"{"type":"lyrics","text":"la"}"#.into(),
                encode(&Event::Volume { volume: 0.5 }).unwrap(),
            ],
        );
        assert_eq!(next(&rx), Msg::Event(Box::new(snapshot("First"))));
        assert_eq!(
            next(&rx),
            Msg::Event(Box::new(Event::Volume { volume: 0.5 }))
        );
        // the daemon quits: its socket goes away
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
        assert_eq!(next(&rx), Msg::Gone);
        // and a new one starts
        let listener = UnixListener::bind(&socket).unwrap();
        serve_once(&listener, &[encode(&snapshot("Again")).unwrap()]);
        assert_eq!(next(&rx), Msg::Event(Box::new(snapshot("Again"))));
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_request_gets_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("melodie.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let srv = std::thread::spawn(move || {
            for answer in [Response::ok(), Response::err("out of the queue")] {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                assert_eq!(line.trim(), r#"{"cmd":"queue_move","from":3,"to":0}"#);
                let mut w = &stream;
                w.write_all(format!("{}\n", encode(&answer).unwrap()).as_bytes())
                    .unwrap();
            }
        });
        let req = Request::QueueMove { from: 3, to: 0 };
        request(&socket, &req).unwrap();
        let e = request(&socket, &req).unwrap_err();
        assert_eq!(e.to_string(), "out of the queue");
        srv.join().unwrap();
        assert!(request(&dir.path().join("none.sock"), &req).is_err());
    }
}
