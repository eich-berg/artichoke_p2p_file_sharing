use crate::onion_service::ONION_ADDRESS;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::channel;
use std::thread;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileInfo {
    pub id: String,
    pub name: String,
    file_type: String,
    size: u64,
    pub url: String
}

pub type FileMap = HashMap<String, FileInfo>;

fn generate_id() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn get_file_extension(filename: &str) -> String {
    Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
        .unwrap_or_else(|| "unknown".to_string())
}

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce
};
use base64::{engine::general_purpose, Engine as _};

fn encrypt_url(url: &str) -> String {
    // In a real app, use a unique key stored in secrets
    let key_bytes = [0u8; 32]; // 256-bit key
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).unwrap();
    let nonce = Nonce::from_slice(&[0u8; 12]); // 96-bit nonce
    
    let ciphertext = cipher.encrypt(nonce, url.as_bytes()).unwrap_or_default();
    general_purpose::STANDARD.encode(ciphertext)
}

pub fn scan_directory(dir_path: &Path) -> FileMap {
    let mut file_map = FileMap::new();

    if let Ok(entries) = fs::read_dir(dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let original_name = path.file_name().unwrap().to_string_lossy().to_string();
                let new_name = original_name.replace(' ', "_");
                let current_path = if original_name.contains(' ') {
                    let new_path = path.with_file_name(&new_name);
                    if let Err(e) = fs::rename(&path, &new_path) {
                        eprintln!("Failed to rename {}: {}", original_name, e);
                        continue; // Skip if rename fails
                    }
                    new_path
                } else {
                    path
                };

                let metadata = match fs::metadata(&current_path) {
                    Ok(meta) => meta,
                    Err(e) => {
                        eprintln!("Failed to get metadata for {}: {}", new_name, e);
                        continue;
                    }
                };

                let id = generate_id();
                let file_info = FileInfo {
                    id: id.clone(),
                    name: new_name.clone(),
                    file_type: get_file_extension(&new_name),
                    size: metadata.len(),
                    url: encrypt_url(&ONION_ADDRESS.get().cloned().unwrap_or_else(|| "N/A".to_string()))
                };

                file_map.insert(id, file_info);
            }
        }
    }

    file_map
}

pub fn save_file_map(file_map: &FileMap, output_path: &Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(file_map)?;
    let mut file = File::create(output_path)?;
    file.write_all(json.as_bytes())?;
    Ok(())
}

pub fn start_file_watcher(shared_dir: &Path, output_file: &Path) -> thread::JoinHandle<()> {
    let shared_dir = shared_dir.to_path_buf();
    let output_file = output_file.to_path_buf();

    thread::spawn(move || {

        // Set up file watcher
        let (tx, rx) = channel();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
            match res {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        eprintln!("Channel closed, stopping watcher");
                    }
                }
                Err(e) => eprintln!("Watch error: {:?}", e),
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("Failed to create watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&shared_dir, RecursiveMode::Recursive) {
            eprintln!("Failed to watch directory: {}", e);
            return;
        }

        // Process events in the background thread
        for event in rx {
            match &event.kind {
                EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_) => {
                    let file_map = scan_directory(&shared_dir);
                    if let Err(e) = save_file_map(&file_map, &output_file) {
                        eprintln!("Error saving file map: {}", e);
                    }
                }
                _ => {} // Ignore other event types
            }
        }
    })
}


