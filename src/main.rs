mod file_watcher;
mod mqtt_sync;
mod onion_service;
mod onion_client;

use file_watcher::{start_file_watcher, scan_directory};
use mqtt_sync::{start_mqtt_sync, get_metadata_uploads};
use onion_service::{arti_axum_add};
use onion_client::{arti_hyper_get};
use std::path::Path;
use std::fs;
use std::io::{self, Write};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal;
use comfy_table::{Table, ContentArrangement};
use comfy_table::presets::{UTF8_FULL, UTF8_BORDERS_ONLY};
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;

static BANNER: &[&str] = &[
    "=============================================WELCOME TO=============================================",
    "  :@@@@@@@@@@@@@@@@@@@%=.          .-@@@@@@@@@+.   .-@@@@@@@@@+.           -#@@@@@@@@@@@@@@@@@@@-   ",
    "  :@@@@@@@@@@@@@@@@@@@@@@@.       :@@@@@@@@@@@@@= :@@@@@@@@@@@@@=       .%@@@@@@@@@@@@@@@@@@@@@@-   ",
    "  :@@@@@@@@@@@@@@@@@@@@@@@@*.    .@@@@@@%:-@@@@@@-@@@@@@+:+@@@@@@:     -@@@@@@@@@@@@@@@@@@@@@@@@-   ",
    "  :@@@@@@@.        .*@@@@@@@*    -@@@@@*    @@@@@%@@@@@.   :@@@@@+    -@@@@@@@#.         *@@@@@@-   ",
    "  :@@@@@@@.          :@@@@@@@:   :@@@@@*    @@@@@@@@@@@.   :@@@@@=   .@@@@@@@=           *@@@@@@-   ",
    "  :@@@@@@@.          .#@@@@@@=   .%@@@@@    @@@@@@@@@@@.   %@@@@@:   :@@@@@@@.           *@@@@@@-   ",
    "  :@@@@@@@.           *@@@@@@=    :@@@@@@.  .::::.:::::   #@@@@@-    :@@@@@@@.           *@@@@@@-   ",
    "  :@@@@@@@.          .@@@@@@@:     .@@@@@@=.            :@@@@@@:     .@@@@@@@:           *@@@@@@-   ",
    "  :@@@@@@@.         :@@@@@@@*.      .+@@@@@@:         .@@@@@@%.       -@@@@@@@=          *@@@@@@-   ",
    "  :@@@@@@@##+*++**%#@@@@@@@#.         .#@@@@@@.     .#@@@@@@.          =@@@@@@@%%**+++**#%@@@@@@-   ",
    "  :@@@@@@@@@@@@@@@@@@@@@@@:             .%@@@@@=   :@@@@@@.             .%@@@@@@@@@@@@@@@@@@@@@@-   ",
    "  :@@@@@@@@@@@@@@@@@@@@#.                 .@@@@@+ -@@@@@+                  +@@@@@@@@@@@@@@@@@@@@-   ",
    "  :@@@@@@@-----:::::.                      .@@@@@:%@@@@+                      ..::::-----*@@@@@@-   ",
    "  :@@@@@@@.                   .:::------::::@@@@@*@@@@@-::::------:::.                   *@@@@@@-   ",
    "  :@@@@@@@.                   =@@@@@@@@@@@@@@@@@@#@@@@@@@@@@@@@@@@@@@#                   *@@@@@@-   ",
    "  :@@@@@@@.                   =@@@@@@@@@@@@@@@@@@#@@@@@@@@@@@@@@@@@@@#                   *@@@@@@-   ",
    "  :@@@@@@@.                   =@@@@@@@@@@@@@@@@@@#@@@@@@@@@@@@@@@@@@@#                   *@@@@@@-   ",
    "  ........                   .........................................                   ........   ",
    "====================================================================================================",
    "",
    ];

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce
};
use base64::{engine::general_purpose, Engine as _};

async fn cleanup_files() {
    let empty_json = b"{}";
    fs::write("local_file_map.json", empty_json).ok();
    fs::write("global_file_map.json", empty_json).ok();
}

fn decrypt_url(encrypted_url: &str) -> String {
    let key_bytes = [0u8; 32];
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).unwrap();
    let nonce = Nonce::from_slice(&[0u8; 12]);
    
    let ciphertext = general_purpose::STANDARD.decode(encrypted_url).unwrap_or_default();
    match cipher.decrypt(nonce, ciphertext.as_slice()) {
        Ok(plaintext) => String::from_utf8_lossy(&plaintext).to_string(),
        Err(_) => "error_decrypting".to_string(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    // Spawn a task to handle Ctrl+C
    tokio::spawn(async {
        signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
        println!("\nReceived Ctrl+C. Cleaning up...");
        if let Some(tx) = onion_service::SHUTDOWN_TX.get() {
            let _ = tx.send(());
            // Give the onion service a moment to drop
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        cleanup_files().await;
        println!("Goodbye!");
        std::process::exit(0);
    });

    for i in BANNER {
        println!("\x1b[38;5;146m{}\x1b[0m", i);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut menu_table = Table::new();
    menu_table
        .load_preset(UTF8_BORDERS_ONLY)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["MENU"])
        .add_row(vec!["    Available commands (case-sensitive):"])
        .add_row(vec!["      !uploads  -> List available file metadata from peers"])
        .add_row(vec!["      !get hash -> Download a file from peer by its hash"])
        .add_row(vec!["      !share    -> Launch service to start file-sharing"])
        .add_row(vec!["      quit      -> Exit the application"])
        .add_row(vec![""])
        .add_row(vec!["    To share a file with peers, manually drop it in the 'shared_files' directory."])
        .add_row(vec!["    Changes in 'shared_files' are tracked automatically."]);

    let mut info_table = Table::new();
    info_table
        .load_preset(UTF8_BORDERS_ONLY)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["INFO"])        
        .add_row(vec!["   [i] Launching sharing service is only needed once per session."])
        .add_row(vec!["   [i] Additions/deletions in 'shared_files' are detected automatically."])
        .add_row(vec!["   [i] Metadata sync for peer uploads runs every 5 secs as a background task."]);

    println!("\x1b[38;5;96m{}\x1b[0m", menu_table);
    println!();
    println!("\x1b[38;5;95m{}\x1b[0m", info_table);
    println!();

    // Create shared_files directory if it doesn't exist
    let shared_dir: &Path = Path::new("shared_files");
    if !shared_dir.exists() {
        fs::create_dir(shared_dir)?;
    }

    // Create initial file_map.json
    let output_file = Path::new("local_file_map.json");    
    
    // Implement MQTT client to sync local_file_map.json with peers
    let (mqtt, collected_metadata, peer_id) = start_mqtt_sync().await?;

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line: String = String::new();

    let mut sharing_initialized = false;

    loop {

        line.clear();
        print!("\x1b[38;5;151m> \x1b[0m");
        io::stdout().flush()?;

        reader.read_line(&mut line).await?;
        let cmd = line.trim();
        match cmd {
                "!uploads" => {
                    if let Err(e) = get_metadata_uploads(mqtt.clone(), collected_metadata.clone(), peer_id.clone()).await {
                        eprintln!("\x1b[38;5;167mError syncing metadata: {}\x1b[0m", e);
                        continue;
                    }
                    let bytes = match tokio::fs::read("global_file_map.json").await {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("\x1b[38;5;167mError reading global map: {}\x1b[0m", e);
                            continue;
                        }
                    };
                    let json_string = String::from_utf8(bytes)?;
                    let json_data: serde_json::Value = serde_json::from_str(&json_string)?;

                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(ContentArrangement::Dynamic)
                        .set_header(vec!["Hash", "Filename", "Type", "Size"]);

                    // we assume JSON structure is always valid
                    let peers = json_data.as_array().unwrap();

                    for peer_entry in peers {
                        let peer = peer_entry.as_object().unwrap();
                        let files = peer.get("files").unwrap().as_object().unwrap();

                        for (_file_id, file_info) in files {
                            let info = file_info.as_object().unwrap();
                            let hash = info.get("id").unwrap().as_str().unwrap();
                            let filename = info.get("name").unwrap().as_str().unwrap();
                            let file_type = info.get("file_type").unwrap().as_str().unwrap();
                            let size = info.get("size").unwrap().as_u64().unwrap();

                            table.add_row(vec![
                                hash.to_string(),
                                filename.to_string(),
                                file_type.to_string(),
                                format!("{:.2} MB", size as f64 / 1_000_000.0) // Convert to MB, 2 decimal places
                            ]);
                        }
                    }

                    if table.row_iter().count() == 0 {
                        println!("No files available from peers.");
                        continue;
                    } else {
                        println!("\x1b[38;5;147m{}\x1b[0m", table);
                    }
                }
                "!share" => {
                    if sharing_initialized {
                        println!("\x1b[38;5;167mSharing already initialized for this session.\x1b[0m");
                        continue;
                    }
                    // Create a progress bar
                    let pb = ProgressBar::new_spinner();
                    pb.set_style(ProgressStyle::default_spinner());
                    pb.set_message("Setting up onion service");
                    pb.enable_steady_tick(Duration::from_millis(100));

                    // Wait for the onion service to be completely added
                    // match arti_axum_add().await {
                    //     Ok(_) => {
                    //         pb.finish_with_message("✓ Onion service ready.");
                    //     },
                    //     Err(e) => {
                    //         pb.finish_and_clear();
                    //         eprintln!("Failed to add onion service: {}", e);
                    //         continue;
                    //     }
                    // }
                    if let Err(e) = arti_axum_add().await {
                        pb.finish_and_clear();
                        eprintln!("\x1b[38;5;167mFailed to add onion service: {}\x1b[0m", e);
                        continue; // Prevents crashing and returns to > prompt
                    }

                    // Now spawn background tasks for scanning and watching
                    sharing_initialized = true;
                    let sd = shared_dir.to_path_buf();
                    let of = output_file.to_path_buf();
                    let initial_file_map = scan_directory(&sd);
                    let json_string = serde_json::to_string_pretty(&initial_file_map).unwrap();
                    fs::write(&of, json_string).ok();
                    
                    // use spawn_blocking to avoid blocking the Tokio executor
                    tokio::task::spawn_blocking(move || {
                        start_file_watcher(&sd, &of);
                    });

                    println!("\x1b[38;5;3m✓ Onion service and file tracker running in the background.\x1b[0m");
                }
                cmd if cmd.starts_with("!get ") => {
                    let hash = cmd[5..].trim();
                    if hash.is_empty() {
                        println!("Usage: !get hash");
                        continue;
                    }
                    // Read and parse the JSON file
                    let json_content = fs::read_to_string("global_file_map.json")
                        .map_err(|e| format!("Failed to read JSON file: {}", e))?;

                    // global_file_map.json is an array of FileEntry { peer_id, files }
                    use crate::mqtt_sync::FileEntry;
                    let file_entries: Vec<FileEntry> = serde_json::from_str(&json_content)
                        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

                    // Find the file info by hash in peer file maps
                    let found_info = file_entries
                        .iter()
                        .find_map(|entry| entry.files.get(hash));

                    match found_info {
                        Some(info) => {
                            // Decrypt the URL before use
                            let real_url = decrypt_url(&info.url);
                            // Construct the destination URL
                            let destination = format!("http://{}/static/{}", real_url, info.name);
                            let downloads: &Path = Path::new("downloads");
                            if !downloads.exists() {
                                fs::create_dir(downloads)?;
                            }
                            // println!("Downloading {} to {:?}...", destination, downloads);
                            if let Err(e) = arti_hyper_get(&destination, downloads).await {
                                eprintln!("\x1b[38;5;167mDownload failed: {}\x1b[0m", e);
                            }
                        }
                        None => {
                            println!("Hash '{}' not found.", hash);
                        }
                    }
                }
                "exit" | "quit" => {
                    if let Some(tx) = onion_service::SHUTDOWN_TX.get() {
                        let _ = tx.send(());
                        // Wait long enough for the message to print before the process dies
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                    }
                    cleanup_files().await;
                    println!("Goodbye!");
                    break;
                }
                _ => {
                    println!("Unknown command: {}", cmd);
                    continue
                }
            }
        }
    Ok(())
}