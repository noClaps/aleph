use crate::{
    adapters::DebugAdapterBinary,
    transport::{IoKind, LogKind, TransportDelegate},
};
use anyhow::Result;
use dap_types::{
    messages::{Message, Response},
    requests::Request,
};
use futures::channel::oneshot;
use gpui::AsyncApp;
use std::{
    hash::Hash,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SessionId(pub u32);

impl SessionId {
    pub fn from_proto(client_id: u64) -> Self {
        Self(client_id as u32)
    }

    pub fn to_proto(self) -> u64 {
        self.0 as u64
    }
}

/// Represents a connection to the debug adapter process, either via stdout/stdin or a socket.
pub struct DebugAdapterClient {
    id: SessionId,
    sequence_count: AtomicU64,
    binary: DebugAdapterBinary,
    transport_delegate: TransportDelegate,
}

pub type DapMessageHandler = Box<dyn FnMut(Message) + 'static + Send + Sync>;

impl DebugAdapterClient {
    pub async fn start(
        id: SessionId,
        binary: DebugAdapterBinary,
        message_handler: DapMessageHandler,
        cx: &mut AsyncApp,
    ) -> Result<Self> {
        let transport_delegate = TransportDelegate::start(&binary, cx).await?;
        let this = Self {
            id,
            binary,
            transport_delegate,
            sequence_count: AtomicU64::new(1),
        };
        this.connect(message_handler, cx).await?;

        Ok(this)
    }

    pub fn should_reconnect_for_ssh(&self) -> bool {
        self.transport_delegate.tcp_arguments().is_some()
            && self.binary.command.as_deref() == Some("ssh")
    }

    pub async fn connect(
        &self,
        message_handler: DapMessageHandler,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        self.transport_delegate.connect(message_handler, cx).await
    }

    pub async fn create_child_connection(
        &self,
        session_id: SessionId,
        binary: DebugAdapterBinary,
        message_handler: DapMessageHandler,
        cx: &mut AsyncApp,
    ) -> Result<Self> {
        let binary = if let Some(connection) = self.transport_delegate.tcp_arguments() {
            DebugAdapterBinary {
                command: None,
                arguments: Default::default(),
                envs: Default::default(),
                cwd: Default::default(),
                connection: Some(connection),
                request_args: binary.request_args,
            }
        } else {
            self.binary.clone()
        };

        Self::start(session_id, binary, message_handler, cx).await
    }

    /// Send a request to an adapter and get a response back
    /// Note: This function will block until a response is sent back from the adapter
    pub async fn request<R: Request>(&self, arguments: R::Arguments) -> Result<R::Response> {
        let serialized_arguments = serde_json::to_value(arguments)?;

        let (callback_tx, callback_rx) = oneshot::channel::<Result<Response>>();

        let sequence_id = self.next_sequence_id();

        let request = crate::messages::Request {
            seq: sequence_id,
            command: R::COMMAND.to_string(),
            arguments: Some(serialized_arguments),
        };
        self.transport_delegate
            .pending_requests
            .lock()
            .insert(sequence_id, callback_tx)?;

        log::debug!(
            "Client {} send `{}` request with sequence_id: {}",
            self.id.0,
            R::COMMAND,
            sequence_id
        );

        self.send_message(Message::Request(request)).await?;

        let command = R::COMMAND.to_string();

        let response = callback_rx.await??;
        log::debug!(
            "Client {} received response for: `{}` sequence_id: {}",
            self.id.0,
            command,
            sequence_id
        );
        match response.success {
            true => {
                if let Some(json) = response.body {
                    Ok(serde_json::from_value(json)?)
                // Note: dap types configure themselves to return `None` when an empty object is received,
                // which then fails here...
                } else if let Ok(result) =
                    serde_json::from_value(serde_json::Value::Object(Default::default()))
                {
                    Ok(result)
                } else {
                    Ok(serde_json::from_value(Default::default())?)
                }
            }
            false => anyhow::bail!("Request failed: {}", response.message.unwrap_or_default()),
        }
    }

    pub async fn send_message(&self, message: Message) -> Result<()> {
        self.transport_delegate.send_message(message).await
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn binary(&self) -> &DebugAdapterBinary {
        &self.binary
    }

    /// Get the next sequence id to be used in a request
    pub fn next_sequence_id(&self) -> u64 {
        self.sequence_count.fetch_add(1, Ordering::Relaxed)
    }

    pub fn kill(&self) {
        log::debug!("Killing DAP process");
        self.transport_delegate.transport.lock().kill();
        self.transport_delegate.pending_requests.lock().shutdown();
    }

    pub fn has_adapter_logs(&self) -> bool {
        self.transport_delegate.has_adapter_logs()
    }

    pub fn add_log_handler<F>(&self, f: F, kind: LogKind)
    where
        F: 'static + Send + FnMut(IoKind, Option<&str>, &str),
    {
        self.transport_delegate.add_log_handler(f, kind);
    }
}
