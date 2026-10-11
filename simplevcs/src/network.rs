use crate::error::{VcsError, VcsResult};
use crate::repo::Repo;
use axum::{routing::{post, get}, Router, extract::{State, Path}, Json, http::{HeaderMap, StatusCode}, body::StreamBody};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::io::ReaderStream;

#[derive(Serialize, Deserialize, Debug)]
pub struct UploadRequest { pub haves: Vec<String>, pub wants: Vec<String>, pub depth: Option<usize> }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkObject { pub hash: String, pub data: Vec<u8> }

pub async fn run_server(repo: Repo, bind: &str, port: u16, cert_path: Option<String>, key_path: Option<String>) -> VcsResult<()> {
    let state = Arc::new(repo);
    let app = Router::new()
        .route("/info/refs", get(handle_info_refs))
        .route("/upload-pack", post(handle_upload_pack))
        .route("/receive-pack", post(handle_receive_pack))
        .route("/lfs/:oid", get(handle_lfs_download).post(handle_lfs_upload))
        .with_state(state);
    
    let addr = format!("{}:{}", bind, port);
    tracing::info!("Server listening on http://{}", addr);
    
    if let (Some(cert), Some(key)) = (cert_path, key_path) {
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await.map_err(|e| VcsError::TlsConfig(e.to_string()))?;
        axum_server::bind_rustls(addr.parse().unwrap(), tls_config)
            .serve(app.into_make_service())
            .await.map_err(|e| VcsError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
    } else {
        tracing::warn!("Running without TLS. Auth tokens will be transmitted in plaintext!");
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    }
    Ok(())
}

fn check_auth(db: &sled::Db, headers: &HeaderMap) -> Result<(), VcsError> {
    if let Some(auth) = headers.get("Authorization") {
        if let Ok(s) = auth.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                for item in db.scan_prefix(b"user:") {
                    if let Ok((_, v)) = item {
                        if String::from_utf8_lossy(&v) == token { return Ok(()); }
                    }
                }
            }
        }
    }
    Err(VcsError::Unauthorized)
}

async fn handle_info_refs(State(repo): State<Arc<Repo>>, headers: HeaderMap) -> Result<String, VcsError> {
    check_auth(&repo.db, &headers)?;
    Ok(repo.read_head()?)
}

async fn handle_receive_pack(State(repo): State<Arc<Repo>>, headers: HeaderMap, body: axum::body::Bytes) -> Result<String, VcsError> {
    check_auth(&repo.db, &headers)?;
    let objects: Vec<NetworkObject> = bincode::deserialize(&zstd::decode_all(&body[..])?)?;
    let count = repo.apply_network_objects(objects)?;
    Ok(format!("Push successful. Received {} objects.", count))
}

async fn handle_upload_pack(State(repo): State<Arc<Repo>>, headers: HeaderMap, Json(req): Json<UploadRequest>) -> Result<Vec<u8>, VcsError> {
    check_auth(&repo.db, &headers)?;
    let packfile = repo.pack_objects(&req.wants[0])?;
    Ok(packfile)
}

async fn handle_lfs_download(State(repo): State<Arc<Repo>>, Path(oid): Path<String>, headers: HeaderMap) -> Result<StreamBody<ReaderStream<tokio::fs::File>>, VcsError> {
    check_auth(&repo.db, &headers)?;
    let lfs_path = repo.lfs_dir().join(&oid);
    if !lfs_path.exists() { return Err(VcsError::FileNotFound(oid)); }
    let file = tokio::fs::File::open(lfs_path).await.map_err(VcsError::Io)?;
    Ok(StreamBody::new(ReaderStream::new(file)))
}

async fn handle_lfs_upload(State(repo): State<Arc<Repo>>, Path(oid): Path<String>, headers: HeaderMap, body: axum::body::Bytes) -> Result<StatusCode, VcsError> {
    check_auth(&repo.db, &headers)?;
    let lfs_path = repo.lfs_dir().join(&oid);
    if !lfs_path.exists() {
        std::fs::create_dir_all(repo.lfs_dir())?;
        std::fs::write(&lfs_path, &body)?;
    }
    Ok(StatusCode::OK)
}

async fn shutdown_signal() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler"); };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("failed to install signal handler").recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {}, }
}
