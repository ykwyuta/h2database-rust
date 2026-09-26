use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{error, info};

use h2_graph::GraphEngine;
use h2_types::H2Result;

use crate::session::BoltSession;

pub struct BoltServer {
    listener: TcpListener,
    engine: Arc<GraphEngine>,
    session_counter: AtomicU64,
}

impl BoltServer {
    pub async fn bind(addr: &str, engine: Arc<GraphEngine>) -> H2Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        Ok(Self {
            listener,
            engine,
            session_counter: AtomicU64::new(1),
        })
    }

    pub fn local_addr(&self) -> H2Result<SocketAddr> {
        self.listener.local_addr().map_err(Into::into)
    }

    pub async fn run(self, mut shutdown: broadcast::Receiver<()>) -> H2Result<()> {
        let local_addr = self.local_addr()?;
        info!("Neo4j Bolt server listening on {}", local_addr);

        loop {
            tokio::select! {
                res = self.listener.accept() => {
                    match res {
                        Ok((stream, peer_addr)) => {
                            let session_id = format!("bolt-{}", self.session_counter.fetch_add(1, Ordering::SeqCst));
                            let engine = Arc::clone(&self.engine);

                            tokio::spawn(async move {
                                let mut session = BoltSession::new(stream, engine, session_id);
                                if let Err(e) = session.run().await {
                                    error!("Bolt session error from {}: {:?}", peer_addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Bolt accept error: {:?}", e);
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("Bolt server shutting down");
                    break;
                }
            }
        }

        Ok(())
    }
}
