use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

const ENVOY_CONFIG_FILENAME: &str = "envoy.yaml";

/// Manages the Envoy proxy child process.
pub struct EnvoyManager {
    child: Child,
    config_path: PathBuf,
}

impl EnvoyManager {
    /// Generate the Envoy config, then spawn Envoy as a child process.
    pub async fn start(config_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("Failed to create config dir: {}", config_dir.display()))?;

        let config_path = config_dir.join(ENVOY_CONFIG_FILENAME);
        write_envoy_config(&config_path)?;

        let child = Command::new("envoy")
            .args(["-c", config_path.to_str().unwrap(), "--log-level", "warning"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn Envoy. Is it installed?")?;

        info!("Envoy started with PID {}", child.id().unwrap_or(0));

        Ok(Self {
            child,
            config_path,
        })
    }

    /// Stop the Envoy process gracefully.
    pub async fn stop(&mut self) {
        info!("Stopping Envoy...");
        if let Err(e) = self.child.kill().await {
            warn!("Failed to kill Envoy: {}", e);
        }
        match self.child.wait().await {
            Ok(status) => info!("Envoy exited with {}", status),
            Err(e) => error!("Failed to wait for Envoy: {}", e),
        }
        // Clean up config file (best effort)
        let _ = std::fs::remove_file(&self.config_path);
    }
}

fn write_envoy_config(path: &Path) -> Result<()> {
    let config = r#"static_resources:
  listeners:
    # Listener 1: Transparent HTTP proxy (redirected port 80 traffic)
    - name: http_transparent
      address:
        socket_address:
          address: 0.0.0.0
          port_value: 10080
      use_original_dst: true
      filter_chains:
        - filters:
            - name: envoy.filters.network.http_connection_manager
              typed_config:
                "@type": type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager
                stat_prefix: http_proxy
                http_filters:
                  - name: envoy.filters.http.router
                    typed_config:
                      "@type": type.googleapis.com/envoy.extensions.filters.http.router.v3.Router
                route_config:
                  virtual_hosts:
                    - name: all
                      domains: ["*"]
                      routes:
                        - match:
                            prefix: "/"
                          route:
                            cluster: original_dst

    # Listener 2: Decrypted HTTPS from MITM proxy
    - name: https_decrypted
      address:
        socket_address:
          address: 127.0.0.1
          port_value: 10081
      filter_chains:
        - filters:
            - name: envoy.filters.network.http_connection_manager
              typed_config:
                "@type": type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager
                stat_prefix: https_proxy
                http_filters:
                  - name: envoy.filters.http.dynamic_forward_proxy
                    typed_config:
                      "@type": type.googleapis.com/envoy.extensions.filters.http.dynamic_forward_proxy.v3.FilterConfig
                      dns_cache_config:
                        name: dns_cache
                        dns_lookup_family: V4_ONLY
                  - name: envoy.filters.http.router
                    typed_config:
                      "@type": type.googleapis.com/envoy.extensions.filters.http.router.v3.Router
                route_config:
                  virtual_hosts:
                    - name: all
                      domains: ["*"]
                      routes:
                        - match:
                            prefix: "/"
                          route:
                            cluster: dynamic_forward_tls

  clusters:
    - name: original_dst
      type: ORIGINAL_DST
      lb_policy: CLUSTER_PROVIDED
      original_dst_lb_config:
        use_http_header: false

    - name: dynamic_forward_tls
      lb_policy: CLUSTER_PROVIDED
      cluster_type:
        name: envoy.clusters.dynamic_forward_proxy
        typed_config:
          "@type": type.googleapis.com/envoy.extensions.clusters.dynamic_forward_proxy.v3.ClusterConfig
          dns_cache_config:
            name: dns_cache
            dns_lookup_family: V4_ONLY
      transport_socket:
        name: envoy.transport_sockets.tls
        typed_config:
          "@type": type.googleapis.com/envoy.extensions.transport_sockets.tls.v3.UpstreamTlsContext
          common_tls_context:
            validation_context:
              trusted_ca:
                filename: /etc/ssl/certs/ca-certificates.crt
"#;

    std::fs::write(path, config)
        .with_context(|| format!("Failed to write Envoy config to {}", path.display()))?;

    info!("Envoy config written to {}", path.display());
    Ok(())
}
