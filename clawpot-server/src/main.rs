mod agent;
mod events;
mod grpc;
mod network;
mod proxy;
mod telemetry;
mod vm;

use anyhow::{Context, Result};
use clawpot_common::proto::clawpot_service_server::ClawpotServiceServer;
use events::{EventStore, PersistMode};
use grpc::ClawpotServiceImpl;
use network::{ip_allocator::IpAllocator, NetworkManager};
use proxy::auth_client::AuthClient;
use proxy::body_store::BodyStore;
use proxy::ca::CertificateAuthority;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::Mutex;
use tonic::transport::Server;
use tracing::{error, info, warn};
use uuid::Uuid;
use vm::VmRegistry;

#[tokio::main]
async fn main() -> Result<()> {
    // Install ring as the default CryptoProvider before any TLS usage.
    // Required because both ring and aws-lc-rs features are enabled via rustls defaults.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install default CryptoProvider");

    // Generate session ID for this server run
    let session_id = Uuid::new_v4().to_string();

    // Initialize telemetry (stdout logging + OTLP export)
    let tracer_provider =
        telemetry::init_telemetry(&session_id).expect("Failed to initialize telemetry");

    info!("Starting clawpot-server (session {})...", session_id);

    // Check if running as root
    if !nix::unistd::geteuid().is_root() {
        error!("Server must be run as root (use sudo)");
        anyhow::bail!(
            "Server must be run as root (sudo required for TAP devices, bridge, and iptables)"
        );
    }

    info!("Running as root");

    // Server configuration - resolve paths relative to the binary or use env override
    let project_root = std::env::var("CLAWPOT_ROOT")
        .map_or_else(|_| PathBuf::from("/workspaces/clawpot"), PathBuf::from);

    // Initialize event store
    let events_db_path = std::env::var("CLAWPOT_EVENTS_DB")
        .map_or_else(|_| project_root.join("data/events.db"), PathBuf::from);
    let persist_mode = PersistMode::from_env();

    let auth_addr = std::env::var("CLAWPOT_AUTH_ADDR").ok();

    let event_store = EventStore::new(
        &events_db_path,
        &session_id,
        env!("CARGO_PKG_VERSION"),
        &serde_json::json!({
            "root": project_root.to_string_lossy(),
            "auth_addr": auth_addr,
        })
        .to_string(),
        persist_mode,
    )
    .context("Failed to initialize event store")?;

    clawpot_event!(event_store, "server.started", "server", {
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "config_root": project_root.to_string_lossy().to_string(),
        "auth_addr": auth_addr
    });

    // Initialize networking
    let network_manager =
        Arc::new(NetworkManager::new().context("Failed to create network manager")?);

    clawpot_log!(event_store, "server", "Ensuring network bridge exists...");
    network_manager
        .ensure_bridge()
        .await
        .context("Failed to ensure bridge exists")?;

    clawpot_log!(event_store, "server", "Network bridge ready");

    // Initialize CA
    let ca_dir = project_root.join("ca");
    let ca = Arc::new(CertificateAuthority::new(&ca_dir).context("Failed to initialize CA")?);
    clawpot_log!(event_store, "server", "Certificate authority ready");

    // Initialize body store
    let body_store_dir = project_root.join("data/bodies");
    let body_store =
        Arc::new(BodyStore::new(&body_store_dir).context("Failed to initialize body store")?);
    clawpot_log!(event_store, "server", "Body store ready");

    // Initialize authorization client
    let auth = Arc::new(
        AuthClient::new(auth_addr.as_deref())
            .await
            .context("Failed to initialize auth client")?,
    );
    clawpot_log!(event_store, "server", "Authorization client ready");

    // Initialize LLM key store
    let llm_keys = Arc::new(proxy::llm::LlmKeyStore::from_env());
    clawpot_log!(event_store, "server", "LLM key store initialized");

    // Create shared cancellation channel
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    // Initialize IP allocator and VM registry (before proxies so registry is available)
    let ip_allocator = Arc::new(Mutex::new(IpAllocator::new()));
    clawpot_log!(
        event_store,
        "server",
        "IP allocator initialized (192.168.100.2-254)"
    );

    let vm_registry = Arc::new(VmRegistry::new());
    clawpot_log!(event_store, "server", "VM registry initialized");

    // Create oneshot channels for proxy startup verification
    let (mitm_ready_tx, mitm_ready_rx) = tokio::sync::oneshot::channel();
    let (http_ready_tx, http_ready_rx) = tokio::sync::oneshot::channel();
    let (dns_ready_tx, dns_ready_rx) = tokio::sync::oneshot::channel();

    // Start TLS MITM proxy
    let mitm_ca = ca.clone();
    let mitm_cancel = cancel_rx.clone();
    let _mitm_handle = tokio::spawn(async move {
        proxy::tls_mitm::run(mitm_ca, mitm_cancel, mitm_ready_tx).await;
    });

    // Start HTTP proxy
    let http_registry = vm_registry.clone();
    let http_events = event_store.clone();
    let http_body_store = body_store.clone();
    let http_auth = auth.clone();
    let http_llm_keys = llm_keys.clone();
    let http_cancel = cancel_rx.clone();
    let _http_handle = tokio::spawn(async move {
        if let Err(e) = proxy::http_proxy::run(
            http_registry,
            http_events,
            http_body_store,
            http_auth,
            http_llm_keys,
            http_cancel,
            http_ready_tx,
        )
        .await
        {
            error!("HTTP proxy failed: {:#}", e);
        }
    });

    // Start DNS proxy
    let dns_registry = vm_registry.clone();
    let dns_events = event_store.clone();
    let dns_auth = auth.clone();
    let dns_cancel = cancel_rx.clone();
    let _dns_handle = tokio::spawn(async move {
        proxy::dns_proxy::run(dns_registry, dns_events, dns_auth, dns_cancel, dns_ready_tx).await;
    });

    // Wait for all proxies to be ready before starting gRPC
    mitm_ready_rx
        .await
        .context("TLS MITM proxy failed to start")?;
    clawpot_log!(event_store, "server", "TLS MITM proxy started");

    http_ready_rx.await.context("HTTP proxy failed to start")?;
    clawpot_log!(event_store, "server", "HTTP proxy started");

    dns_ready_rx.await.context("DNS proxy failed to start")?;
    clawpot_log!(event_store, "server", "DNS proxy started");

    let kernel_path = project_root.join("assets/kernels/vmlinux");
    let rootfs_path = project_root.join("assets/rootfs/ubuntu.ext4");

    // Verify assets exist
    if !kernel_path.exists() {
        error!("Kernel not found at {:?}", kernel_path);
        anyhow::bail!(
            "Kernel image not found. Run './scripts/install-vm-assets.sh' to download assets."
        );
    }

    if !rootfs_path.exists() {
        error!("Rootfs not found at {:?}", rootfs_path);
        anyhow::bail!(
            "Rootfs image not found. Run './scripts/install-vm-assets.sh' to download assets."
        );
    }

    clawpot_log!(event_store, "server", "VM assets verified");

    // Create gRPC service
    let service = ClawpotServiceImpl::new(
        vm_registry.clone(),
        ip_allocator.clone(),
        network_manager.clone(),
        kernel_path,
        rootfs_path,
        event_store.clone(),
    );

    // Bind address
    let addr = "0.0.0.0:50051".parse()?;
    clawpot_log!(event_store, "server", "Starting gRPC server on {}", addr);

    // Start gRPC server with graceful shutdown
    Server::builder()
        .add_service(ClawpotServiceServer::new(service))
        .serve_with_shutdown(
            addr,
            shutdown_signal(
                vm_registry,
                network_manager,
                ip_allocator,
                cancel_tx,
                event_store.clone(),
            ),
        )
        .await
        .context("gRPC server failed")?;

    clawpot_event!(event_store, "server.stopped", "server", {
        "reason": "shutdown"
    });

    event_store.close_session().await;

    info!("Server shut down successfully");

    // Flush remaining spans before exit
    if let Err(e) = tracer_provider.shutdown() {
        error!("Failed to shut down tracer provider: {}", e);
    }

    Ok(())
}

/// Graceful shutdown handler
#[tracing::instrument(name = "server.shutdown", skip_all)]
async fn shutdown_signal(
    registry: Arc<VmRegistry>,
    network_manager: Arc<NetworkManager>,
    ip_allocator: Arc<Mutex<IpAllocator>>,
    cancel_tx: tokio::sync::watch::Sender<bool>,
    event_store: EventStore,
) {
    // Wait for SIGINT (Ctrl+C) or SIGTERM
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("Failed to register SIGTERM handler");

    tokio::select! {
        result = signal::ctrl_c() => {
            match result {
                Ok(()) => clawpot_log!(event_store, "server", "Received SIGINT, initiating graceful shutdown..."),
                Err(err) => {
                    error!("Failed to listen for SIGINT: {}", err);
                    return;
                }
            }
        }
        _ = sigterm.recv() => {
            clawpot_log!(event_store, "server", "Received SIGTERM, initiating graceful shutdown...");
        }
    }

    // Cleanup all VMs
    clawpot_log!(event_store, "server", "Cleaning up all VMs...");

    let vms_list = registry.list().await;
    clawpot_log!(
        event_store,
        "server",
        "Found {} VMs to clean up",
        vms_list.len()
    );

    for (vm_id, ip_address, tap_name, _, _, _) in vms_list {
        let _cleanup_span = tracing::info_span!("shutdown.cleanup_vm", vm_id = %vm_id).entered();
        clawpot_log!(event_store, "server", vm_id = vm_id, "Cleaning up VM");

        // Remove from registry and stop VM
        match registry.remove(&vm_id).await {
            Ok(mut entry) => {
                // Stop VM
                if let Err(e) = entry.manager.stop().await {
                    warn!("Failed to stop VM {}: {}", vm_id, e);
                }

                // Delete TAP device
                if let Err(e) = network_manager.delete_tap(&tap_name, ip_address).await {
                    warn!("Failed to delete TAP device {}: {}", tap_name, e);
                }

                // Release IP
                if let Err(e) = ip_allocator.lock().await.release(ip_address) {
                    warn!("Failed to release IP {}: {}", ip_address, e);
                }

                clawpot_log!(
                    event_store,
                    "server",
                    vm_id = vm_id,
                    "VM cleaned up successfully"
                );
            }
            Err(e) => {
                warn!("Failed to remove VM {} from registry: {}", vm_id, e);
            }
        }
    }

    // Stop proxy infrastructure
    clawpot_log!(event_store, "server", "Stopping proxy infrastructure...");
    let _ = cancel_tx.send(true);
    network::iptables::remove_proxy_rules(network_manager.bridge_name());

    clawpot_log!(
        event_store,
        "server",
        "All VMs cleaned up. Server shutting down."
    );
}
