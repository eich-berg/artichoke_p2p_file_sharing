/*
reference used: https://gitlab.torproject.org/tpo/core/arti/-/tree/main/examples/axum/axum-hello-world
*/

use arti_client::{TorClient, TorClientConfig};
use anyhow::Result;
use hyper::Request;
use once_cell::sync::OnceCell;
use axum::Router;
use axum::routing::get;
use futures::StreamExt;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server;
use tower::Service;
use safelog::{DisplayRedacted as _, sensitive};
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::StreamRequest;
use tor_hsservice::config::OnionServiceConfigBuilder;
use tor_proto::client::stream::IncomingStreamRequest;
use tower_http::services::ServeDir;
use tokio::sync::broadcast;

pub static ONION_ADDRESS: OnceCell<String> = OnceCell::new();
pub static SHUTDOWN_TX: OnceCell<broadcast::Sender<()>> = OnceCell::new();

pub async fn arti_axum_add() -> anyhow::Result<()> {

    // tracing_subscriber::fmt::init();

    let (tx, mut rx) = broadcast::channel(1);
    SHUTDOWN_TX.set(tx).ok();

    let serve_dir = ServeDir::new("shared_files");
    let router = Router::new()
        .route("/", get(|| async { "Hello world! 🌍" }))
        .nest_service("/static", serve_dir);

    // Set environment variable to disable permissions checks, for ex. Replit's 
    // shared environment has directory permissions that Arti considers "unsafe".
    unsafe {
        std::env::set_var("FS_MISTRUST_DISABLE_PERMISSIONS_CHECKS", "1");
    }

    let config = TorClientConfig::default();
    let client = TorClient::create_bootstrapped(config).await.unwrap();

    let svc_cfg = OnionServiceConfigBuilder::default()
        .nickname("orange-electron".parse().unwrap())
        .build()
        .unwrap();


    if let Some((service, request_stream)) = client.launch_onion_service(svc_cfg).unwrap() {
        eprintln!("\x1b[48;5;3m[DEBUG TXT] {}\x1b[0m", service.onion_address().unwrap().display_unredacted());
        ONION_ADDRESS.set(service.onion_address().unwrap().display_unredacted().to_string()).unwrap();
        eprintln!("\x1b[48;5;3m[DEBUG TXT] waiting for service to become fully reachable\x1b[0m");

        while let Some(status) = service.status_events().next().await {
            // eprintln!("Service status: {:?}", status);             
            if status.state().is_fully_reachable() {
                break;
            }
        }

        let stream_requests = tor_hsservice::handle_rend_requests(request_stream);
        eprintln!("\x1b[48;5;3m[DEBUG TXT] ready to serve connections\x1b[0m");

        // Spawn the request handling loop in the background so we can return to main
        tokio::spawn(async move {
            tokio::pin!(stream_requests);
            loop {
                tokio::select! {
                    res = stream_requests.next() => {
                        if let Some(stream_request) = res {
                            let router = router.clone();
                            tokio::spawn(async move {
                                let request = stream_request.request().clone();
                                if let Err(err) = handle_stream_request(stream_request, router).await {
                                    eprintln!("error serving connection {:?}: {}", sensitive(request), err);
                                };
                            });
                        } else {
                            break;
                        }
                    }
                    _ = rx.recv() => {
                        eprintln!("Shutdown signal received, closing onion service...");
                        break;
                    }
                }
            }
            drop(service);
            eprintln!("onion service exited cleanly");
        });
    } else {
        eprintln!("onion service was disabled in config");
    }
    Ok(())
}

async fn handle_stream_request(stream_request: StreamRequest, router: Router) -> Result<()> {
    match stream_request.request() {
        IncomingStreamRequest::Begin(begin) if begin.port() == 80 => {
            let onion_service_stream = stream_request.accept(Connected::new_empty()).await?;
            let io = TokioIo::new(onion_service_stream);

            let hyper_service = hyper::service::service_fn(move |request: Request<Incoming>| {
                router.clone().call(request)
            });

            server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
                .map_err(|x| anyhow::anyhow!(x))?;
        }
        _ => {
            stream_request.shutdown_circuit()?;
        }
    }

    Ok(())
}