use std::{
    future::Future, io::Write, marker::PhantomData, process::Stdio, sync::Arc, time::Duration,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use rmcp::{
    service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{async_rw::JsonRpcMessageCodec, Transport},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::{Child, Command},
    sync::Mutex,
};
use tokio_util::codec::{Encoder, FramedRead, FramedWrite};

type ClientReader<R> = FramedRead<R, JsonRpcMessageCodec<RxJsonRpcMessage<RoleClient>>>;
type ClientWriter<W> = FramedWrite<W, BoundedJsonRpcEncoder<TxJsonRpcMessage<RoleClient>>>;

/// Official-rmcp JSON decoding plus a hard inbound/outbound line limit.
pub(crate) struct BoundedAsyncRwTransport<R, W> {
    reader: ClientReader<R>,
    writer: Arc<Mutex<Option<ClientWriter<W>>>>,
}

impl<R, W> BoundedAsyncRwTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    pub(crate) fn new(reader: R, writer: W, max_message_bytes: usize) -> Self {
        Self {
            reader: FramedRead::new(
                reader,
                JsonRpcMessageCodec::new_with_max_length(max_message_bytes),
            ),
            writer: Arc::new(Mutex::new(Some(FramedWrite::new(
                writer,
                BoundedJsonRpcEncoder::new(max_message_bytes),
            )))),
        }
    }
}

struct BoundedJsonRpcEncoder<T> {
    maximum: usize,
    marker: PhantomData<fn() -> T>,
}

impl<T> BoundedJsonRpcEncoder<T> {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            marker: PhantomData,
        }
    }
}

impl<T: serde::Serialize> Encoder<T> for BoundedJsonRpcEncoder<T> {
    type Error = std::io::Error;

    fn encode(&mut self, item: T, destination: &mut BytesMut) -> Result<(), Self::Error> {
        let start = destination.len();
        let mut writer = BoundedBytesWriter {
            destination,
            maximum: self.maximum,
            written: 0,
        };
        if let Err(error) = serde_json::to_writer(&mut writer, &item) {
            writer.destination.truncate(start);
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
        writer.destination.extend_from_slice(b"\n");
        Ok(())
    }
}

struct BoundedBytesWriter<'a> {
    destination: &'a mut BytesMut,
    maximum: usize,
    written: usize,
}

impl Write for BoundedBytesWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(buffer.len()) > self.maximum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "MCP stdio message exceeded configured limit",
            ));
        }
        self.destination.extend_from_slice(buffer);
        self.written = self.written.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<R, W> Transport<RoleClient> for BoundedAsyncRwTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = Arc::clone(&self.writer);
        async move {
            let mut writer = writer.lock().await;
            let writer = writer.as_mut().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "MCP stdio writer is closed",
                )
            })?;
            writer.send(item).await
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        match self.reader.next().await {
            Some(Ok(message)) => Some(message),
            Some(Err(error)) => {
                tracing::warn!(
                    error_class = "bounded_stdio_frame_rejected",
                    error = %error,
                    "closing MCP stdio transport after invalid or oversized frame"
                );
                None
            },
            None => None,
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.writer.lock().await.take();
        Ok(())
    }
}

/// A bounded stdio transport that owns and deterministically reaps its server child.
pub(crate) struct BoundedChildProcess {
    io: BoundedAsyncRwTransport<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    child: Option<Child>,
    pid: Option<u32>,
}

const CHILD_TERMINATION_GRACE: Duration = Duration::from_millis(250);

impl BoundedChildProcess {
    pub(crate) fn spawn(mut command: Command, max_message_bytes: usize) -> std::io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.as_std_mut().process_group(0);
        let mut child = command.spawn()?;
        let pid = child.id();
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("spawned MCP server did not expose piped stdout")
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::other("spawned MCP server did not expose piped stdin")
        })?;
        Ok(Self {
            io: BoundedAsyncRwTransport::new(stdout, stdin, max_message_bytes),
            child: Some(child),
            pid,
        })
    }
}

impl Transport<RoleClient> for BoundedChildProcess {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.io.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.io.receive().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.io.close().await?;
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        if child.try_wait()?.is_none() {
            terminate_process_group(self.pid, false);
            match tokio::time::timeout(CHILD_TERMINATION_GRACE, child.wait()).await {
                Ok(result) => {
                    let _ = result?;
                },
                Err(_) => {
                    terminate_process_group(self.pid, true);
                    child.start_kill()?;
                    let _ = child.wait().await?;
                },
            }
        }
        // The launcher can exit before descendants. Kill the remaining owned group
        // even after the direct child has already been observed/reaped.
        terminate_process_group(self.pid, true);
        self.pid = None;
        Ok(())
    }
}

impl Drop for BoundedChildProcess {
    fn drop(&mut self) {
        terminate_process_group(self.pid, true);
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

#[cfg(unix)]
fn terminate_process_group(pid: Option<u32>, force: bool) {
    let Some(pid) = pid.and_then(|value| i32::try_from(value).ok()) else {
        return;
    };
    // SAFETY: `spawn` creates a new process group whose leader is the owned child.
    unsafe {
        libc::kill(-pid, if force { libc::SIGKILL } else { libc::SIGTERM });
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: Option<u32>, _force: bool) {}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{
        fs,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    use rmcp::transport::Transport;
    use tokio::io::{split, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn oversized_stdio_frame_is_rejected_before_json_decoding() {
        let (client, server) = tokio::io::duplex(512);
        let (client_read, client_write) = split(client);
        let (_, mut server_write) = split(server);
        let mut transport = BoundedAsyncRwTransport::new(client_read, client_write, 64);

        server_write.write_all(&[b'x'; 65]).await.unwrap();
        server_write.write_all(b"\n").await.unwrap();

        assert!(Transport::<RoleClient>::receive(&mut transport)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn bounded_stdio_accepts_a_small_valid_json_rpc_message() {
        let (client, server) = tokio::io::duplex(512);
        let (client_read, client_write) = split(client);
        let (_, mut server_write) = split(server);
        let mut transport = BoundedAsyncRwTransport::new(client_read, client_write, 512);

        server_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"result":{}}
"#,
            )
            .await
            .unwrap();

        assert!(Transport::<RoleClient>::receive(&mut transport)
            .await
            .is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_transport_close_terminates_the_owned_process_group() {
        let pid_file = std::env::temp_dir().join(format!(
            "magician-mcp-child-{}-{}.pid",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let script = format!(
            "/bin/sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; wait",
            pid_file.display()
        );
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let mut transport = BoundedChildProcess::spawn(command, 1024).expect("spawn group");
        let deadline = Instant::now() + Duration::from_secs(1);
        let descendant_pid = loop {
            if let Ok(value) = fs::read_to_string(&pid_file) {
                if let Ok(pid) = value.parse::<i32>() {
                    break pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "descendant pid was not published"
            );
            tokio::task::yield_now().await;
        };

        Transport::<RoleClient>::close(&mut transport)
            .await
            .expect("close group");
        let gone_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            // SAFETY: signal zero performs a read-only existence check.
            let alive = unsafe { libc::kill(descendant_pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(Instant::now() < gone_deadline, "descendant escaped cleanup");
            tokio::task::yield_now().await;
        }
        let _ = fs::remove_file(pid_file);
    }

    #[tokio::test]
    async fn oversized_outbound_stdio_frame_is_rejected_without_partial_write() {
        let (client, mut server) = tokio::io::duplex(512);
        let (client_read, client_write) = split(client);
        let mut transport = BoundedAsyncRwTransport::new(client_read, client_write, 64);
        let message: TxJsonRpcMessage<RoleClient> = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "custom/oversized",
            "params": {"payload": "x".repeat(128)}
        }))
        .unwrap();

        assert!(Transport::<RoleClient>::send(&mut transport, message)
            .await
            .is_err());
        let mut observed = [0; 1];
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            tokio::io::AsyncReadExt::read(&mut server, &mut observed),
        )
        .await
        .is_err());
    }
}
