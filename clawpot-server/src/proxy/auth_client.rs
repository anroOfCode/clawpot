use anyhow::{Context, Result};
use clawpot_common::network_auth_proto::{
    network_authorization_request,
    network_authorization_service_client::NetworkAuthorizationServiceClient, DnsRequest,
    HttpRequest, NetworkAuthorizationRequest,
};
use tonic::transport::Channel;
use tracing::{info, warn};

const MAX_BODY_FOR_GRPC: usize = 1024 * 1024; // 1MB

/// Client for the external Python authorization service.
/// If no address is configured, all requests are allowed.
pub enum AuthClient {
    Connected(NetworkAuthorizationServiceClient<Channel>),
    Disabled,
}

impl AuthClient {
    /// Connect to the authorization service, or disable if addr is None.
    pub async fn new(addr: Option<&str>) -> Result<Self> {
        if let Some(addr) = addr {
            let client = NetworkAuthorizationServiceClient::connect(addr.to_string())
                .await
                .with_context(|| format!("Failed to connect to auth service at {addr}"))?;
            info!("Connected to authorization service at {}", addr);
            Ok(AuthClient::Connected(client))
        } else {
            info!("No CLAWPOT_AUTH_ADDR set, authorization disabled (allow-all)");
            Ok(AuthClient::Disabled)
        }
    }

    /// Authorize an HTTP request. Returns (allowed, reason).
    pub async fn authorize_http(
        &self,
        request_id: i64,
        vm_id: &str,
        method: &str,
        url: &str,
        headers: &std::collections::HashMap<String, String>,
        body: &[u8],
    ) -> Result<(bool, String)> {
        match self {
            AuthClient::Disabled => Ok((true, "authorization disabled".to_string())),
            AuthClient::Connected(client) => {
                let truncated = body.len() > MAX_BODY_FOR_GRPC;
                let body_bytes = if truncated {
                    body[..MAX_BODY_FOR_GRPC].to_vec()
                } else {
                    body.to_vec()
                };

                let request = NetworkAuthorizationRequest {
                    request_id: request_id.to_string(),
                    vm_id: vm_id.to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    request: Some(network_authorization_request::Request::Http(HttpRequest {
                        method: method.to_string(),
                        url: url.to_string(),
                        headers: headers.clone(),
                        body: body_bytes,
                        body_truncated: truncated,
                    })),
                };

                let mut client = client.clone();
                match client.authorize(request).await {
                    Ok(resp) => {
                        let resp = resp.into_inner();
                        Ok((resp.allow, resp.reason))
                    }
                    Err(e) => {
                        warn!("Auth service call failed (denying): {}", e);
                        Ok((false, format!("auth service unreachable: {e}")))
                    }
                }
            }
        }
    }

    /// Authorize a DNS request. Returns (allowed, reason).
    pub async fn authorize_dns(
        &self,
        request_id: i64,
        vm_id: &str,
        query_name: &str,
        query_type: &str,
    ) -> Result<(bool, String)> {
        match self {
            AuthClient::Disabled => Ok((true, "authorization disabled".to_string())),
            AuthClient::Connected(client) => {
                let request = NetworkAuthorizationRequest {
                    request_id: request_id.to_string(),
                    vm_id: vm_id.to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    request: Some(network_authorization_request::Request::Dns(DnsRequest {
                        query_name: query_name.to_string(),
                        query_type: query_type.to_string(),
                    })),
                };

                let mut client = client.clone();
                match client.authorize(request).await {
                    Ok(resp) => {
                        let resp = resp.into_inner();
                        Ok((resp.allow, resp.reason))
                    }
                    Err(e) => {
                        warn!("Auth service call failed (denying): {}", e);
                        Ok((false, format!("auth service unreachable: {e}")))
                    }
                }
            }
        }
    }
}
