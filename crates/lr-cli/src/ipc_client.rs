//! The CLI end of a local IPC channel: the hub's CLI channel, the broker's admin channel and a
//! runner's control channel (spec 3.9, 13.3). The server is checked before anything is sent.

use lr_abstractions::{Dialer, Frame, Transport, TransportError};
use lr_net::{IpcDialer, ServerIdentity};
use lr_proto::PROTOCOL_VERSION;
use lr_proto::{receive, IpcChannel, IpcOpen, IpcOpenReply, Message, PeerKind, Received};
use std::time::Duration;

/// How long the CLI waits for the server to open the channel.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// An open local channel.
pub struct IpcClient {
    t: Box<dyn Transport>,
}

fn io(e: TransportError) -> String {
    e.to_string()
}

impl IpcClient {
    /// Dials `address`, accepting a server run by this OS user or by the machine's
    /// administrators (a broker service), and opens `channel`.
    pub async fn connect(address: &str, channel: IpcChannel) -> Result<IpcClient, String> {
        let server = ServerIdentity::current_user()
            .map_err(|e| e.to_string())?
            .or_admins();
        IpcClient::connect_to(address, channel, server).await
    }

    /// Dials the broker's admin channel at `address`: its server may also be the broker
    /// service's account (spec 3.9).
    pub async fn connect_broker_admin(address: &str) -> Result<IpcClient, String> {
        let server = crate::os_service::broker_servers()?;
        IpcClient::connect_to(address, IpcChannel::Admin, server).await
    }

    /// Dials `address`, accepting only a server run by `server`, and opens `channel`.
    pub async fn connect_to(
        address: &str,
        channel: IpcChannel,
        server: ServerIdentity,
    ) -> Result<IpcClient, String> {
        let t = IpcDialer::new(server)
            .dial(address, None)
            .await
            .map_err(|e| format!("{address}: {e}"))?;
        IpcClient::open(t, channel).await
    }

    /// Opens `channel` on an already dialed transport.
    pub async fn open(t: Box<dyn Transport>, channel: IpcChannel) -> Result<IpcClient, String> {
        let client = IpcClient { t };
        client
            .send(&IpcOpen::Hello {
                version: PROTOCOL_VERSION,
                channel,
            })
            .await?;
        let reply = tokio::time::timeout(OPEN_TIMEOUT, client.t.recv())
            .await
            .map_err(|_| "the server did not open the channel in time".to_string())?
            .map_err(io)?;
        match lr_proto::decode::<IpcOpenReply>(reply.as_bytes()).map_err(|e| e.to_string())? {
            IpcOpenReply::Ready { .. } => Ok(client),
            IpcOpenReply::Refused(e) => Err(format!("refused: {}", e.message)),
        }
    }

    /// Sends one message.
    pub async fn send<M: Message>(&self, msg: &M) -> Result<(), String> {
        let frame = Frame::json(msg).map_err(|e| e.to_string())?;
        self.t.send(frame).await.map_err(io)
    }

    /// The next message of type `M`; messages of an unknown newer type are skipped.
    pub async fn recv<M: Message>(&self) -> Result<M, String> {
        loop {
            let frame = self.t.recv().await.map_err(io)?;
            match receive::<M>(frame.as_bytes(), PeerKind::Ipc).map_err(|e| e.to_string())? {
                Received::Message(m) => return Ok(m),
                Received::Ignored(_) => continue,
            }
        }
    }

    /// Sends `req` and returns the next reply.
    pub async fn call<Q: Message, R: Message>(&self, req: &Q) -> Result<R, String> {
        self.send(req).await?;
        self.recv().await
    }

    /// Closes the channel.
    pub async fn close(self) {
        self.t.close().await;
    }
}
