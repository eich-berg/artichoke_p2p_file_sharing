/*
references used:
- https://gitlab.torproject.org/tpo/core/arti/-/tree/main/examples/hyper-examples
- https://gitlab.torproject.org/tpo/core/arti/-/blob/main/crates/arti-client/examples/lazy-init.rs
- https://gitlab.torproject.org/tpo/core/arti/-/blob/main/crates/arti-client/examples/readme-with-report.rs
*/

use arti_client::{IsolationToken, StreamPrefs, TorClient, TorClientConfig};
use tor_rtcompat::PreferredRuntime;
use tor_error::ErrorReport;
use std::fs::File;
use std::io::Write;
use anyhow::Result;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::http::uri::Scheme;
use hyper::{Request, Uri};
use tokio::io::{AsyncRead, AsyncWrite};
use once_cell::sync::OnceCell;
use hyper_util::rt::TokioIo;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use std::path::Path;
use chrono;
use anyhow::anyhow;
use std::os::unix::fs::FileExt;
use futures::future::join_all;

// note this might be replaced with OnceLock in the future, refer to arti example page
pub static TOR_CLIENT: OnceCell<TorClient<PreferredRuntime>> = OnceCell::new();

pub async fn get_arti_client() -> Result<TorClient<PreferredRuntime>> {
    let client = TOR_CLIENT.get_or_try_init(|| -> Result<TorClient<_>> {
        unsafe {
            std::env::set_var("FS_MISTRUST_DISABLE_PERMISSIONS_CHECKS", "1");
        }
        let config = TorClientConfig::default();
        Ok(TorClient::builder().config(config).create_unbootstrapped()?)
    })?;

    // Only bootstrap if it hasn't been done yet
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner());
    pb.set_message("Bootstrapping Tor client...");
    pb.enable_steady_tick(Duration::from_millis(100));

    match client.bootstrap().await {
        Ok(()) => {
            pb.finish_with_message("✓ Connected");
        }
        Err(err) => {
            pb.finish_and_clear();
            eprintln!("Failed to bootstrap Tor client: {}", err.report());
            return Err(err.into());
        }
    }

    Ok(client.clone())
}
    
async fn send_request(host: &str, path: &str, output_file: &str, stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> Result<()> {

    let (mut request_sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;

    tokio::spawn(async move {
        let _ = connection.await;
    });

    eprintln!("\x1b[48;5;3m[DEBUG TXT] [+] Making request to host\x1b[0m");

    let mut resp = request_sender
    .send_request(
        Request::builder()
            .header("Host", host)
            .uri(path)
            .method("GET")
            .body(Empty::<Bytes>::new())?,
    )
    .await?;

    eprintln!("\x1b[48;5;3m[DEBUG TXT] [+] Response status: {}\x1b[0m", resp.status());

    let mut file = File::create(output_file)?;

    while let Some(frame) = resp.body_mut().frame().await {
        let bytes = frame?.into_data().unwrap();
        file.write_all(&bytes)?;
    }

    Ok(())
}


// pub async fn arti_hyper_get(url: &str, downloads_dir: &Path) -> Result<()> {  

//     let arti_client = get_arti_client().await?;

//     // Extract filename from URL or use a default
//     let file_name = if let Ok(parsed_url) = url.parse::<Uri>() {
//         parsed_url
//             .path()
//             .rsplit('/')
//             .next()
//             .filter(|s| !s.is_empty())
//             .unwrap_or("downloaded_file")
//             .to_string()
//     } else {
//         // If URL parsing fails, use a timestamp-based filename
//         format!("downloaded_{}", chrono::Local::now().timestamp())
//     };

//     let output_path = downloads_dir.join(file_name);
//     let output_file = output_path.to_str().unwrap();

//     // Create a progress bar for the download
//     let pb = ProgressBar::new_spinner();
//     pb.set_style(ProgressStyle::default_spinner());
//     pb.set_message("Downloading file...");
//     pb.enable_steady_tick(Duration::from_millis(100));

//     // run the existing download logic in an inner async block so we can capture the Result
//     let res: Result<()> = (async {
//         let parsed_url: Uri = url.parse()?;
//         let host = parsed_url.host().unwrap();
//         let path = parsed_url
//             .path_and_query()
//             .map(|pq| pq.as_str())
//             .unwrap_or("/");
//         let https = parsed_url.scheme() == Some(&Scheme::HTTPS);
//         let port = match parsed_url.port_u16() {
//             Some(port) => port,
//             None if https => 443,
//             None => 80,
//         };

//         let mut prefs = StreamPrefs::new();
//         prefs.set_isolation(IsolationToken::new());

//         // pb.set_message(format!("Connecting to {}:{}...", host, port));

//         let stream = arti_client
//             .connect_with_prefs((host, port), &prefs)
//             .await?;

//         if https {
//             pb.set_message("Establishing TLS connection...");
//             let cx = tokio_native_tls::native_tls::TlsConnector::builder().build()?;
//             let cx = tokio_native_tls::TlsConnector::from(cx);
//             let stream = cx.connect(host, stream).await?;
//             send_request(host, path, output_file, stream).await?;
//         } else {
//             send_request(host, path, output_file, stream).await?;
//         }

//         Ok(())
//     })
//     .await;

//     match res {
//         Ok(()) => {
//             pb.finish_with_message(format!("✓ Downloaded to {}", output_file));
//             Ok(())       
//         }
//         Err(e) => {
//             pb.finish_and_clear();
//             println!("✗ Download error: {}", e);
//             Err(e)
//         }
//     }
// }

pub async fn arti_hyper_get(url: &str, downloads_dir: &Path) -> Result<()> {
    let arti_client = get_arti_client().await?;
    let num_workers = 4; 

    // 1. Setup metadata
    let parsed_url: Uri = url.parse()?;
    let host = parsed_url.host().unwrap().to_string();
    let port = parsed_url.port_u16().unwrap_or(80);
    let path_str = parsed_url.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

    let file_name = path_str.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("downloaded_file");
    let output_path = downloads_dir.join(file_name);

    let file = std::fs::File::create(&output_path)?;
    let arc_file = std::sync::Arc::new(file);

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner());
    pb.set_message("Initializing multi-worker download...");
    pb.enable_steady_tick(Duration::from_millis(100));

    // 2. Initial HEAD request using TokioIo wrapper
    let stream = arti_client.connect((host.clone(), port)).await?;
    let io = TokioIo::new(stream); // Wrap the Arti stream
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(conn);

    let head_req = Request::builder()
        .method("HEAD")
        .uri(path_str) // Pass the string directly
        .header("Host", &host)
        .body(Empty::<Bytes>::new())?;

    let head_resp = sender.send_request(head_req).await?;
    let total_size = head_resp.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        .ok_or_else(|| anyhow!("Server did not provide content-length"))?;

    pb.set_length(total_size);

    // 3. Spawn Worker Tasks
    let chunk_size = total_size / num_workers;
    let mut worker_handles = vec![];

    for i in 0..num_workers {
        let start = i * chunk_size;
        let end = if i == num_workers - 1 { total_size - 1 } else { (i + 1) * chunk_size - 1 };

        let client_clone = arti_client.clone();
        let host_clone = host.clone();
        let path_clone = path_str.to_string();
        let file_clone = arc_file.clone();

        let handle = tokio::spawn(async move {
            let stream = client_clone.connect((host_clone.clone(), port)).await?;
            let io = TokioIo::new(stream); // Wrap for each worker
            let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
            tokio::spawn(conn);

            let req = Request::builder()
                .uri(path_clone)
                .header("Host", host_clone)
                .header("Range", format!("bytes={}-{}", start, end))
                .body(Empty::<Bytes>::new())?;

            let mut resp = sender.send_request(req).await?;
            let mut current_offset = start;

            use http_body_util::BodyExt; // For .frame()
            while let Some(frame) = resp.body_mut().frame().await {
                let frame = frame?;
                if let Ok(data) = frame.into_data() {
                    let len = data.len() as u64;
                    file_clone.write_at(&data, current_offset)?;
                    current_offset += len;
                }
            }
            Ok::<(), anyhow::Error>(())
        });
        worker_handles.push(handle);
    }

    let results = join_all(worker_handles).await;
    for res in results {
        res??; 
    }

    pb.finish_with_message(format!("✓ Multi-worker download complete: {}", output_path.display()));
    Ok(())
}