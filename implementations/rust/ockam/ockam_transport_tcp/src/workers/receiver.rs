use crate::workers::Addresses;
use crate::{TcpRegistry, TcpSendWorkerMsg};
use ockam_core::compat::net::SocketAddr;
use ockam_core::compat::sync::Arc;
use ockam_core::sessions::{SessionId, SessionIdLocalInfo};
use ockam_core::{async_trait, DenyAll, Mailbox, Mailboxes, OutgoingAccessControl};
use ockam_core::{Decodable, LocalMessage, Processor, Result, TransportMessage};
use ockam_node::{Context, ProcessorBuilder};
use ockam_transport_core::TransportError;
use tokio::{io::AsyncReadExt, net::tcp::OwnedReadHalf};
use tracing::{error, info, trace};

/// A TCP receiving message processor
///
/// Create this processor type by calling
/// [`TcpSendWorker::start_pair`](crate::TcpSendWorker::start_pair)
///
/// This half of the worker is created when spawning a new connection
/// worker pair, and listens for incoming TCP packets, to relay into
/// the node message system.
pub(crate) struct TcpRecvProcessor {
    registry: TcpRegistry,
    read_half: OwnedReadHalf,
    peer: SocketAddr,
    addresses: Addresses,
    session_id: Option<SessionId>,
}

impl TcpRecvProcessor {
    /// Create a new `TcpRecvProcessor`
    pub fn new(
        registry: TcpRegistry,
        read_half: OwnedReadHalf,
        peer: SocketAddr,
        addresses: Addresses,
        session_id: Option<SessionId>,
    ) -> Self {
        Self {
            registry,
            read_half,
            peer,
            addresses,
            session_id,
        }
    }

    pub async fn start(
        ctx: &Context,
        registry: TcpRegistry,
        read_half: OwnedReadHalf,
        addresses: &Addresses,
        peer: SocketAddr,
        receiver_outgoing_access_control: Arc<dyn OutgoingAccessControl>,
        session_id: Option<SessionId>,
    ) -> Result<()> {
        let receiver =
            TcpRecvProcessor::new(registry, read_half, peer, addresses.clone(), session_id);

        let mailbox = Mailbox::new(
            addresses.receiver_address().clone(),
            Arc::new(DenyAll),
            receiver_outgoing_access_control,
        );
        ProcessorBuilder::with_mailboxes(Mailboxes::new(mailbox, vec![]), receiver)
            .start(ctx)
            .await?;

        Ok(())
    }
}

#[async_trait]
impl Processor for TcpRecvProcessor {
    type Context = Context;

    async fn initialize(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.set_cluster(crate::CLUSTER_NAME).await?;

        self.registry.add_receiver_processor(&ctx.address());

        Ok(())
    }

    async fn shutdown(&mut self, ctx: &mut Self::Context) -> Result<()> {
        self.registry.remove_receiver_processor(&ctx.address());

        Ok(())
    }

    /// Get the next message from the connection if there are any
    /// available and forward it to the next hop in the route.
    ///
    /// Notes:
    ///
    /// 1. We are using the initialize function here to run a custom loop,
    ///    instead of listening for messages sent to our address.
    /// 2. When the loop exits, we _must_ call stop_worker(..) on
    ///    Context to avoid spawning a zombie task.
    /// 3. We must also stop the TcpReceive loop when the worker gets
    ///    killed by the user or node.
    async fn process(&mut self, ctx: &mut Context) -> Result<bool> {
        // Run in a loop until TcpWorkerPair::stop() is called
        // First read a message length header...
        let len = match self.read_half.read_u16().await {
            Ok(len) => len,
            Err(_e) => {
                info!(
                    "Connection to peer '{}' was closed; dropping stream",
                    self.peer
                );

                // Notify sender tx is closed
                ctx.send(
                    self.addresses.sender_internal_addr().clone(),
                    TcpSendWorkerMsg::ConnectionClosed,
                )
                .await?;

                return Ok(false);
            }
        };

        trace!("Received message header for {} bytes", len);

        // Allocate a buffer of that size
        let mut buf = vec![0; len as usize];

        // Then read into the buffer
        match self.read_half.read_exact(&mut buf).await {
            Ok(_) => {}
            _ => {
                error!("Failed to receive message of length: {}", len);
                return Ok(true);
            }
        }

        // Deserialize the message now
        let mut msg = TransportMessage::decode(&buf).map_err(|_| TransportError::RecvBadMessage)?;

        // Heartbeat message
        if msg.onward_route.next().is_err() {
            trace!("Got heartbeat message from: {}", self.peer);
            return Ok(true);
        }

        // Insert the peer address into the return route so that
        // reply routing can be properly resolved
        msg.return_route
            .modify()
            .prepend(self.addresses.sender_address().clone());

        trace!("Message onward route: {}", msg.onward_route);
        trace!("Message return route: {}", msg.return_route);

        let local_info = match &self.session_id {
            Some(session_id) => vec![SessionIdLocalInfo::new(session_id.clone()).to_local_info()?],
            None => vec![],
        };

        // Forward the message to the next hop in the route
        ctx.forward(LocalMessage::new(msg, local_info)).await?;

        Ok(true)
    }
}
