use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use super::auth_client::AuthClient;
use super::body_store::BodyStore;
use super::db::RequestDb;
use crate::vm::VmRegistry;

const HTTP_LISTEN_ADDR: &str = "0.0.0.0:10080";
const HTTPS_LISTEN_ADDR: &str = "0.0.0.0:10081";

/// Shared context for the HTTP proxy handlers.
struct ProxyCtx {
    registry: Arc<VmRegistry>,
    db: RequestDb,
    body_store: Arc<BodyStore>,
    auth: Arc<AuthClient>,
    use_tls_upstream: bool,
    http_client: Client<hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>, Full<Bytes>>,
}

/// Start both HTTP proxy listeners (plain HTTP + TLS upstream).
pub async fn run(
    registry: Arc<VmRegistry>,
    db: RequestDb,
    body_store: Arc<BodyStore>,
    auth: Arc<AuthClient>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let https_connector = match hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
    {
        Ok(builder) => builder,
        Err(e) => {
            warn!("Failed to load native TLS roots ({}), falling back to webpki roots", e);
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
        }
    }
    .https_or_http()
    .enable_http1()
    .build();

    let http_client = Client::builder(TokioExecutor::new()).build(https_connector);

    // Pre-bind both listeners before spawning tasks
    let http_listener = TcpListener::bind(HTTP_LISTEN_ADDR)
        .await
        .with_context(|| format!("Failed to bind HTTP proxy on {}", HTTP_LISTEN_ADDR))?;
    let https_listener = TcpListener::bind(HTTPS_LISTEN_ADDR)
        .await
        .with_context(|| format!("Failed to bind HTTP proxy on {}", HTTPS_LISTEN_ADDR))?;

    info!("HTTP proxy listening on {} and {}", HTTP_LISTEN_ADDR, HTTPS_LISTEN_ADDR);

    // Signal readiness now that both sockets are bound
    let _ = ready.send(());

    let http_ctx = Arc::new(ProxyCtx {
        registry: registry.clone(),
        db: db.clone(),
        body_store: body_store.clone(),
        auth: auth.clone(),
        use_tls_upstream: false,
        http_client: http_client.clone(),
    });

    let https_ctx = Arc::new(ProxyCtx {
        registry,
        db,
        body_store,
        auth,
        use_tls_upstream: true,
        http_client,
    });

    let mut cancel2 = cancel.clone();

    let http_task = tokio::spawn(async move {
        if let Err(e) = run_listener(http_listener, http_ctx, &mut cancel).await {
            error!("HTTP proxy listener failed: {:#}", e);
        }
    });

    let https_task = tokio::spawn(async move {
        if let Err(e) = run_listener(https_listener, https_ctx, &mut cancel2).await {
            error!("HTTPS proxy listener failed: {:#}", e);
        }
    });

    let _ = tokio::join!(http_task, https_task);
    info!("HTTP proxy shut down");
    Ok(())
}

async fn run_listener(
    listener: TcpListener,
    ctx: Arc<ProxyCtx>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer_addr) = result.context("Failed to accept connection")?;
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let ctx = ctx.clone();
                        async move { handle_request(req, peer_addr, ctx).await }
                    });
                    if let Err(e) = http1::Builder::new()
                        .preserve_header_case(true)
                        .serve_connection(io, service)
                        .await
                    {
                        if !e.to_string().contains("connection closed") {
                            warn!("HTTP connection from {} error: {}", peer_addr, e);
                        }
                    }
                });
            }
            _ = cancel.changed() => {
                info!("HTTP proxy listener received shutdown signal");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_request(
    req: Request<Incoming>,
    peer_addr: SocketAddr,
    ctx: Arc<ProxyCtx>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    match handle_request_inner(req, peer_addr, ctx).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            warn!("Proxy request failed: {:#}", e);
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(format!("Proxy error: {}", e))))
                .unwrap())
        }
    }
}

async fn handle_request_inner(
    req: Request<Incoming>,
    peer_addr: SocketAddr,
    ctx: Arc<ProxyCtx>,
) -> Result<Response<Full<Bytes>>> {
    let start = Instant::now();

    // 1. Resolve vm_id from source IP
    let vm_id = ctx
        .registry
        .find_by_ip(peer_addr.ip())
        .await
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // 2. Extract request metadata
    let method = req.method().to_string();
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let path = req.uri().path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    let scheme = if ctx.use_tls_upstream { "https" } else { "http" };
    let url = format!("{}://{}{}", scheme, host, path);

    let headers_map: HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let headers_json = serde_json::to_string(&headers_map).unwrap_or_default();

    // Collect request body
    let (parts, body) = req.into_parts();
    let req_body = body
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();

    // 3. Log request
    let stored_body = ctx.body_store.store(0, "req", &req_body).ok();
    let (body_inline, body_path) = match &stored_body {
        Some(super::body_store::StoredBody::Inline(b)) => (Some(b.as_slice()), None),
        Some(super::body_store::StoredBody::External(p)) => (None, p.to_str()),
        None => (None, None),
    };

    let request_id = ctx.db.log_request(
        &vm_id,
        if ctx.use_tls_upstream { "https" } else { "http" },
        Some(&method),
        Some(&url),
        Some(&headers_json),
        None,
        None,
        Some(req_body.len() as i64),
        body_inline,
        body_path,
    ).unwrap_or_else(|e| {
        warn!("Failed to log request: {}", e);
        0
    });

    // Re-store body with correct request_id if it was externalized
    if request_id > 0 {
        if let Some(super::body_store::StoredBody::External(_)) = &stored_body {
            let _ = ctx.body_store.store(request_id, "req", &req_body);
        }
    }

    // 4. Authorize
    let auth_start = Instant::now();
    let (allowed, reason) = ctx
        .auth
        .authorize_http(request_id, &vm_id, &method, &url, &headers_map, &req_body)
        .await
        .unwrap_or((false, "auth error".to_string()));
    let auth_latency = auth_start.elapsed().as_millis() as i64;

    if request_id > 0 {
        let _ = ctx.db.log_authorization(request_id, allowed, &reason, auth_latency);
    }

    // 5. If denied, return 403
    if !allowed {
        let duration_ms = start.elapsed().as_millis() as i64;
        if request_id > 0 {
            let _ = ctx.db.log_response(request_id, Some(403), Some(0), None, None, None, None, duration_ms);
        }
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Full::new(Bytes::from(format!("Denied: {}", reason))))
            .unwrap());
    }

    // 6. Forward to upstream
    let upstream_uri: hyper::Uri = url.parse()
        .with_context(|| format!("Invalid upstream URL: {}", url))?;

    let mut upstream_req = Request::builder()
        .method(parts.method)
        .uri(&upstream_uri);

    for (key, value) in &parts.headers {
        upstream_req = upstream_req.header(key, value);
    }

    let upstream_req = upstream_req
        .body(Full::new(req_body.clone()))
        .context("Failed to build upstream request")?;

    let upstream_resp = ctx.http_client
        .request(upstream_req)
        .await
        .context("Upstream request failed")?;

    let status = upstream_resp.status();
    let resp_headers: HashMap<String, String> = upstream_resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let resp_headers_json = serde_json::to_string(&resp_headers).unwrap_or_default();

    // Collect response body
    let resp_body = upstream_resp
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();

    // 7. Log response
    let duration_ms = start.elapsed().as_millis() as i64;
    if request_id > 0 {
        let stored_resp = ctx.body_store.store(request_id, "resp", &resp_body).ok();
        let (resp_inline, resp_path) = match &stored_resp {
            Some(super::body_store::StoredBody::Inline(b)) => (Some(b.as_slice()), None),
            Some(super::body_store::StoredBody::External(p)) => (None, p.to_str()),
            None => (None, None),
        };

        let _ = ctx.db.log_response(
            request_id,
            Some(status.as_u16() as i32),
            Some(resp_body.len() as i64),
            resp_inline,
            resp_path,
            Some(&resp_headers_json),
            None,
            duration_ms,
        );
    }

    // 8. Return response to VM
    let mut response = Response::builder().status(status);
    for (key, value) in &resp_headers {
        // Skip hop-by-hop headers
        let lower = key.to_lowercase();
        if lower == "transfer-encoding" || lower == "connection" {
            continue;
        }
        response = response.header(key.as_str(), value.as_str());
    }

    Ok(response
        .body(Full::new(resp_body))
        .unwrap())
}
