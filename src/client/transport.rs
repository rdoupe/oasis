use super::*;
use std::time::Instant;

pub(super) fn start_endpoint_transport(
    stream: LocalStream,
    lifetime: impl Send + 'static,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    max_frame_size: usize,
    surface_decoder: Option<protocol::surface_reuse::Decoder>,
) -> Result<endpoint::NativeEndpointTransport, ClientError> {
    let reader = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    let transport = endpoint::NativeEndpointTransport::with_lifetime(stream, lifetime)
        .map_err(ClientError::ConnectionFailed)?;
    spawn_server_reader(
        reader,
        transport.stop_handle(),
        event_tx.clone(),
        max_frame_size,
        endpoint_id,
        generation,
        surface_decoder,
    )
    .map_err(ClientError::ConnectionFailed)?;
    Ok(transport)
}

/// Starts reading one endpoint connection. On Unix inside the client's
/// runtime this is a task on the loop's own thread, so an update wakes only
/// that thread; elsewhere a blocking reader thread forwards over the channel.
pub(super) fn spawn_server_reader(
    reader: LocalStream,
    stopped: Arc<AtomicBool>,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    max_frame_size: usize,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    surface_decoder: Option<protocol::surface_reuse::Decoder>,
) -> io::Result<()> {
    #[cfg(unix)]
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let stream = crate::platform::local_stream_into_unix(reader)?;
        let stream = {
            let _entered = runtime.enter();
            tokio::net::UnixStream::from_std(stream)?
        };
        runtime.spawn(server_reader_task(
            stream,
            event_tx,
            stopped,
            max_frame_size,
            endpoint_id,
            generation,
            surface_decoder,
        ));
        return Ok(());
    }
    std::thread::Builder::new()
        .name("endpoint-reader".into())
        .spawn(move || {
            server_reader_thread(
                reader,
                event_tx,
                &stopped,
                max_frame_size,
                endpoint_id,
                generation,
                surface_decoder,
            );
        })
        .map(drop)
}

/// The async reader: frames are read and decoded on the client loop's
/// thread and handed to the loop through the same bounded channel, so a slow
/// loop still stops reading (and the server stops sending) instead of
/// buffering. Stopping the transport shuts the socket down, which ends the
/// read with EOF exactly as the thread reader reports it.
#[cfg(unix)]
pub(super) async fn server_reader_task(
    mut stream: tokio::net::UnixStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    stopped: Arc<AtomicBool>,
    max_frame_size: usize,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    mut surface_decoder: Option<protocol::surface_reuse::Decoder>,
) {
    loop {
        if stopped.load(Ordering::Acquire) {
            break;
        }
        let message = read_frame_async(&mut stream, max_frame_size)
            .await
            .and_then(|message| match &mut surface_decoder {
                Some(decoder) => decoder.decode(message).map_err(|error| {
                    protocol::FramingError::Io(io::Error::new(io::ErrorKind::InvalidData, error))
                }),
                None => Ok(message),
            });
        match message {
            Ok(msg) => {
                if event_tx
                    .send(ClientLoopEvent::ServerMessage {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                        message: Box::new(msg),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(error) => {
                if !matches!(error, protocol::FramingError::UnexpectedEof) {
                    warn!(err = %error, "server read error");
                }
                let _ = event_tx
                    .send(ClientLoopEvent::ServerDisconnected {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                    })
                    .await;
                break;
            }
        }
    }
}

/// Reads one length-prefixed frame and decodes it with the same
/// [`protocol::read_message`] the blocking reader uses.
#[cfg(unix)]
async fn read_frame_async(
    stream: &mut tokio::net::UnixStream,
    max_frame_size: usize,
) -> Result<protocol::ServerMessage, protocol::FramingError> {
    use tokio::io::AsyncReadExt as _;
    let eof_or_io = |error: io::Error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            protocol::FramingError::UnexpectedEof
        } else {
            protocol::FramingError::Io(error)
        }
    };
    let mut frame = vec![0u8; 4];
    stream.read_exact(&mut frame).await.map_err(eof_or_io)?;
    let claimed = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if claimed > max_frame_size {
        return Err(protocol::FramingError::Oversized {
            claimed,
            max: max_frame_size,
        });
    }
    frame.resize(4 + claimed, 0);
    stream
        .read_exact(&mut frame[4..])
        .await
        .map_err(eof_or_io)?;
    protocol::read_message(&mut frame.as_slice(), max_frame_size)
}

/// Reads complete frames while retaining partial-read progress across nonblocking polls.
pub(super) fn server_reader_thread(
    mut stream: LocalStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    mut surface_decoder: Option<protocol::surface_reuse::Decoder>,
) {
    if stream.set_nonblocking(true).is_err() {
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected {
            endpoint_id,
            generation,
        });
        return;
    }

    let mut stream = EndpointReader {
        stream: &mut stream,
        stopped: should_quit,
        deadline: None,
    };
    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        let message = protocol::read_message(&mut stream, max_frame_size).and_then(|message| {
            match &mut surface_decoder {
                Some(decoder) => decoder.decode(message).map_err(|error| {
                    protocol::FramingError::Io(io::Error::new(io::ErrorKind::InvalidData, error))
                }),
                None => Ok(message),
            }
        });
        match message {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                        message: Box::new(msg),
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected {
                    endpoint_id: endpoint_id.clone(),
                    generation,
                });
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected {
                    endpoint_id: endpoint_id.clone(),
                    generation,
                });
                break;
            }
        }
    }
}

pub(super) struct EndpointReader<'a> {
    pub(super) stream: &'a mut LocalStream,
    pub(super) stopped: &'a AtomicBool,
    pub(super) deadline: Option<Instant>,
}

impl io::Read for EndpointReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Ok(0);
            }
            // Without a deadline the wait is unbounded: stopping the transport
            // shuts the socket down, which wakes it.
            let remaining = match self.deadline {
                Some(deadline) => Some(
                    deadline
                        .checked_duration_since(Instant::now())
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::TimedOut, "handshake response timed out")
                        })?,
                ),
                None => None,
            };
            match crate::ipc::poll_local_stream_read_count(self.stream, buffer)? {
                crate::ipc::LocalStreamReadCount::Data(count) => return Ok(count),
                crate::ipc::LocalStreamReadCount::Closed => return Ok(0),
                crate::ipc::LocalStreamReadCount::Pending => {
                    crate::platform::wait_client_stream_readable(self.stream, remaining)?;
                }
            }
        }
    }
}

pub(in crate::client) fn write_to_local_server(
    stream: &mut LocalStream,
    msg: &ClientMessage,
) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|error| io::Error::other(error.to_string()))
}

pub(super) trait ClientMessageSink {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()>;
}

impl ClientMessageSink for LocalStream {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()> {
        write_to_local_server(self, message)
    }
}

impl ClientMessageSink for endpoint::EndpointRegistry {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()> {
        // The lifecycle loop consumes failures for every endpoint, including Local. A send
        // failure must not bypass that transition or tear down unrelated connections.
        self.send(message);
        Ok(())
    }
}

pub(super) fn write_to_server(
    stream: &mut impl ClientMessageSink,
    msg: &ClientMessage,
) -> io::Result<()> {
    stream.send_client_message(msg)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::endpoint::EndpointTransport as _;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{Read as _, Write as _};
    use std::time::Instant;

    #[test]
    fn upload_cancellation_preserves_pending_endpoint_download() {
        let path = std::env::temp_dir().join(format!(
            "herdr-cancel-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let mut bridge = listener.accept().unwrap();
        std::fs::remove_file(path).unwrap();
        drop(listener);
        let mut reader_stream = client.try_clone().unwrap();
        let mut writer = endpoint::NativeEndpointTransport::with_lifetime(client, ()).unwrap();
        let stopped = writer.stop_handle();
        struct ForwardedInput(std::sync::mpsc::Sender<Vec<u8>>);
        impl io::Write for ForwardedInput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (forwarded_tx, forwarded_rx) = std::sync::mpsc::channel();
        let cancel = crate::remote::bridge_upload_cancellation_for_test(
            bridge.try_clone().unwrap(),
            ForwardedInput(forwarded_tx),
        );
        let message = ClientMessage::ClientShellFocus { focused: false };
        let mut expected = Vec::new();
        protocol::write_message(&mut expected, &message).unwrap();
        writer.send(&message).unwrap();
        let mut forwarded = Vec::new();
        while forwarded.len() < expected.len() {
            forwarded.extend(forwarded_rx.recv_timeout(Duration::from_secs(3)).unwrap());
        }
        assert_eq!(forwarded, expected);
        cancel();

        // A client write after upload cancellation must not stop the download reader.
        writer
            .send(&ClientMessage::ClientShellFocus { focused: true })
            .unwrap();
        let flushed = writer.flush(Instant::now() + Duration::from_secs(3));
        if flushed.is_ok() {
            let received: ClientMessage =
                protocol::read_message(&mut bridge, protocol::MAX_FRAME_SIZE).unwrap();
            assert_eq!(received, ClientMessage::ClientShellFocus { focused: true });
        }
        const FINAL: &[u8] = b"pending-download: FINAL OUTPUT\n";
        bridge.write_all(FINAL).unwrap();
        drop(bridge);
        let mut output = Vec::new();
        EndpointReader {
            stream: &mut reader_stream,
            stopped: &stopped,
            deadline: None,
        }
        .read_to_end(&mut output)
        .unwrap();
        assert_eq!(output, FINAL);
        assert!(flushed.is_ok(), "client write failed: {flushed:?}");
        assert!(!stopped.load(Ordering::Acquire));
        assert!(writer.take_error().is_none());
    }
}

#[cfg(all(test, unix))]
mod async_reader_tests {
    use super::*;
    use std::io::Write as _;

    fn shutdown(reason: &str) -> protocol::ServerMessage {
        protocol::ServerMessage::ServerShutdown {
            reason: Some(reason.into()),
        }
    }

    fn frame(message: &protocol::ServerMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        protocol::write_message(&mut bytes, message).unwrap();
        bytes
    }

    fn reason(event: ClientLoopEvent) -> Option<String> {
        match event {
            ClientLoopEvent::ServerMessage { message, .. } => match *message {
                protocol::ServerMessage::ServerShutdown { reason } => reason,
                _ => None,
            },
            _ => None,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn start(
        capacity: usize,
    ) -> (
        std::os::unix::net::UnixStream,
        tokio::sync::mpsc::Receiver<ClientLoopEvent>,
        Arc<AtomicBool>,
        std::os::unix::net::UnixStream,
    ) {
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        let wake = client.try_clone().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let stopped = Arc::new(AtomicBool::new(false));
        spawn_server_reader(
            crate::platform::local_stream_from_unix(client),
            Arc::clone(&stopped),
            tx,
            protocol::MAX_FRAME_SIZE,
            endpoint::ClientEndpointId::Local,
            7,
            None,
        )
        .unwrap();
        (server, rx, stopped, wake)
    }

    #[test]
    fn frames_split_across_writes_arrive_whole_and_in_order() {
        runtime().block_on(async {
            let (mut server, mut rx, _stopped, _wake) = start(8);
            let mut bytes = frame(&shutdown("first"));
            bytes.extend(frame(&shutdown("second")));
            let (head, tail) = bytes.split_at(3);
            server.write_all(head).unwrap();
            tokio::task::yield_now().await;
            server.write_all(tail).unwrap();
            assert_eq!(reason(rx.recv().await.unwrap()).as_deref(), Some("first"));
            assert_eq!(reason(rx.recv().await.unwrap()).as_deref(), Some("second"));
        });
    }

    #[test]
    fn eof_reports_one_disconnect_for_its_generation() {
        runtime().block_on(async {
            let (server, mut rx, _stopped, _wake) = start(8);
            drop(server);
            assert!(matches!(
                rx.recv().await,
                Some(ClientLoopEvent::ServerDisconnected { generation: 7, .. })
            ));
            assert!(
                rx.recv().await.is_none(),
                "the reader ends after one disconnect"
            );
        });
    }

    #[test]
    fn shutting_the_socket_down_wakes_a_waiting_reader() {
        runtime().block_on(async {
            let (_server, mut rx, stopped, wake) = start(8);
            tokio::task::yield_now().await;
            stopped.store(true, Ordering::Release);
            wake.shutdown(std::net::Shutdown::Both).unwrap();
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("reader woke");
            assert!(matches!(
                event,
                Some(ClientLoopEvent::ServerDisconnected { .. })
            ));
        });
    }

    /// A full channel stops the reader instead of buffering frames: the rest
    /// stay in the socket until the loop drains, then arrive in order.
    #[test]
    fn a_full_channel_holds_frames_in_the_socket() {
        runtime().block_on(async {
            let (mut server, mut rx, _stopped, _wake) = start(1);
            for index in 0..4 {
                server
                    .write_all(&frame(&shutdown(&index.to_string())))
                    .unwrap();
            }
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                rx.len(),
                1,
                "one frame buffered; the rest wait in the socket"
            );
            for index in 0..4 {
                let got = reason(rx.recv().await.unwrap());
                assert_eq!(got.as_deref(), Some(index.to_string().as_str()));
            }
        });
    }
}
