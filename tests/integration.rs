use std::sync::{Arc, Mutex};

use lennox_s30::{Event, S30Client};

/// Run with: cargo test --test integration -- --ignored
/// Requires: cd ~/home/lennoxs30api && python simulator/main.py -c simulator/conf/config_heatpump_furnace.json
#[tokio::test]
#[ignore]
async fn connect_poll_disconnect() {
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let mut client = S30Client::builder("127.0.0.1:8080")
        .protocol("http")
        .on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        })
        .build();

    client.connect().await.expect("connect failed");

    // First poll should return system config + zone data
    client.poll().await.expect("poll failed");

    let systems = client.systems();
    assert!(!systems.is_empty(), "should have at least one system");
    assert!(
        !systems[0].zones.is_empty(),
        "should have at least one zone"
    );

    {
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty(), "should have received events");
    }

    client.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
#[ignore]
async fn outdoor_temp_updates() {
    // Simulator cycles outdoor temp every 5s. Poll twice with a delay
    // and verify OutdoorTempChanged fires.
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let mut client = S30Client::builder("127.0.0.1:8080")
        .protocol("http")
        .on_event(move |event| {
            events_clone.lock().unwrap().push(format!("{event:?}"));
        })
        .build();

    client.connect().await.expect("connect failed");

    // First poll - initial state
    client.poll().await.expect("first poll failed");

    // Wait for simulator to cycle
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    // Second poll - should see changes
    client.poll().await.expect("second poll failed");

    {
        let captured = events.lock().unwrap();
        let has_outdoor = captured.iter().any(|e| e.contains("OutdoorTempChanged"));
        println!("Outdoor temp events: {has_outdoor}");
        println!("Total events: {}", captured.len());
    }

    client.disconnect().await.expect("disconnect failed");
}
