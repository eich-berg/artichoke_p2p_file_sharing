/* 
references:
- https://github.com/bytebeamio/rumqtt/blob/main/rumqttc/examples/asyncpubsub.rs
- https://github.com/bytebeamio/rumqtt/blob/main/rumqttc/examples/serde.rs
- https://github.com/bytebeamio/rumqtt/blob/main/rumqttc/examples/syncpubsub.rs
- https://github.com/bytebeamio/rumqtt/blob/main/rumqttc/examples/tls.rs
- https://github.com/bytebeamio/rumqtt/blob/main/rumqttc/examples/tls_native.rs
*/

use serde::{Serialize, Deserialize};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use uuid::Uuid;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::{task, fs};
use std::error::Error;

use crate::file_watcher::FileMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub peer_id: String,
    pub files: FileMap,
}

async fn publish_local_metadata(
    mqtt: &AsyncClient,
    peer_id: &str,
    retained: bool,  // Differentiate between periodic (retained) and request (not retained)
) -> Result<(), Box<dyn Error>> {
    let local_bytes = fs::read("local_file_map.json").await?;
    let local_map: FileMap = serde_json::from_slice(&local_bytes)?;

    let announcement = FileEntry {
        peer_id: peer_id.to_string(),
        files: local_map,
    };

    let payload = serde_json::to_vec(&announcement)?;

    mqtt.publish(
        "artichoke_file_metadata/rumqtt",
        QoS::AtLeastOnce,
        retained,
        payload,
    ).await?;

    Ok(())
}

pub async fn start_mqtt_sync() -> Result<(AsyncClient, Arc<Mutex<Vec<FileEntry>>>, String), Box<dyn Error>> {
    let peer_id = Uuid::new_v4().to_string();

    let mut mqttoptions = MqttOptions::new(
        format!("peer-{}", peer_id),
        "test.mosquitto.org",
        1883 // 8883 for tls, 1883
    );

    // TODO -> mqttoptions.set_transport(Transport::tls_with_default_config());
    mqttoptions.set_keep_alive(Duration::from_secs(30));

    let (mqtt, mut eventloop) = AsyncClient::new(mqttoptions, 10);
    mqtt.subscribe("artichoke_file_metadata/rumqtt", QoS::AtLeastOnce).await?;

    let collected_metadata: Arc<Mutex<Vec<FileEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_clone = collected_metadata.clone();

    // Single task that handles both MQTT events and periodic publishing
    let mqtt_clone = mqtt.clone();
    let peer_id_clone = peer_id.clone();

    task::spawn(async move {
        let mut last_payload: Option<Vec<u8>> = None;
        let mut interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                // Handle MQTT events
                event = eventloop.poll() => {
                    if let Ok(event) = event {
                        if let Event::Incoming(Incoming::Publish(p)) = event {
                            if let Ok(announcement) = serde_json::from_slice::<FileEntry>(&p.payload) {
                                // ignore announcements we authored
                                if announcement.peer_id == peer_id_clone {
                                    continue;
                                }
                                // store or replace remote announcement by peer_id (deduplicate)
                                {
                                    let mut guard = collected_clone.lock().unwrap();
                                    if let Some(idx) = guard.iter().position(|a| a.peer_id == announcement.peer_id) {
                                        guard[idx] = announcement.clone();
                                    } else {
                                        guard.push(announcement.clone());
                                    }
                                } // guard dropped here, before any await

                                // best-effort reply with our current local file_map
                                if let Err(e) = publish_local_metadata(&mqtt_clone, &peer_id_clone, false).await {
                                    eprintln!("Failed to send reply: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }

                // Periodic publishing
                _ = interval.tick() => {
                    if let Ok(bytes) = fs::read("local_file_map.json").await {
                        let should_publish = match &last_payload {
                            Some(prev) => prev.as_slice() != bytes.as_slice(),
                            None => true,
                        };

                        if should_publish {
                            match publish_local_metadata(&mqtt_clone, &peer_id_clone, false).await {
                                Ok(_) => {
                                    // Update last_payload with the bytes we just read
                                    last_payload = Some(bytes);
                                }
                                Err(e) => {
                                    eprintln!("Failed to publish background announcement: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    Ok((mqtt, collected_metadata, peer_id))
}

pub async fn get_metadata_uploads(mqtt: AsyncClient, collected_metadata: Arc<Mutex<Vec<FileEntry>>>, peer_id: String,) -> Result<(), Box<dyn Error>> {

    // Merge old responses instead of clearing to preserve background sync
    collected_metadata.lock().unwrap().clear(); // Remove?

    publish_local_metadata(&mqtt, &peer_id, false).await?;

    // wait briefly for incoming responses/replies
    tokio::time::sleep(Duration::from_secs(3)).await;

    let maps = collected_metadata.lock().unwrap();

    // clone collected maps and persist to disk for main to read and display
    let maps_vec: Vec<FileEntry> = maps
        .iter()
        .filter(|entry| entry.peer_id != peer_id)  // Exclude our own peer_id
        .cloned()
        .collect();
    let json = serde_json::to_vec_pretty(&maps_vec)?;
    fs::write("global_file_map.json", &json).await?;

    Ok(())
}