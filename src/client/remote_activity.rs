//! Reads agent panes' detection text from saved SSH machines whose servers do
//! not report background activity themselves (see `shell::remote_activity`).
//!
//! Each machine gets one worker thread that opens the machine's JSON API bridge
//! on its first read and then blocks on its request channel, so an idle client
//! runs nothing. A machine that cannot serve the read answers `None`, which the
//! shell takes as "stop asking".

use super::*;

/// How long one pane read may take before it counts as unavailable.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads a pane's detection text, `None` when the server cannot serve it.
pub(super) type PaneScreenReader = Box<dyn FnMut(&str) -> Option<String> + Send>;

#[derive(Default)]
pub(super) struct RemoteActivityReaders {
    workers: HashMap<endpoint::ClientEndpointId, std::sync::mpsc::Sender<String>>,
}

impl RemoteActivityReaders {
    /// Queues a read of `pane_id` on `endpoint_id`'s machine. The answer comes
    /// back as [`ClientLoopEvent::RemoteAgentScreen`].
    pub(super) fn read(
        &mut self,
        profiles: &[endpoint::SavedSshEndpoint],
        endpoint_id: endpoint::ClientEndpointId,
        pane_id: String,
        event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    ) {
        if let Some(worker) = self.workers.get(&endpoint_id) {
            match worker.send(pane_id) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(pane_id)) => {
                    self.workers.remove(&endpoint_id);
                    return self.read(profiles, endpoint_id, pane_id, event_tx);
                }
            }
        }
        let endpoint::ClientEndpointId::Ssh(profile_id) = &endpoint_id else {
            let _ = event_tx.try_send(ClientLoopEvent::RemoteAgentScreen {
                endpoint_id,
                pane_id,
                text: None,
            });
            return;
        };
        let Some(profile) = profiles.iter().find(|profile| &profile.id == profile_id) else {
            let _ = event_tx.try_send(ClientLoopEvent::RemoteAgentScreen {
                endpoint_id,
                pane_id,
                text: None,
            });
            return;
        };
        let reader = saved_machine_reader(profile.clone());
        let worker = spawn_worker(endpoint_id.clone(), reader, event_tx.clone());
        let _ = worker.send(pane_id);
        self.workers.insert(endpoint_id, worker);
    }

    /// Drops workers for machines that are no longer saved; their bridges close.
    pub(super) fn retain_profiles(&mut self, profiles: &[endpoint::SavedSshEndpoint]) {
        self.workers.retain(|endpoint_id, _| match endpoint_id {
            endpoint::ClientEndpointId::Ssh(profile_id) => {
                profiles.iter().any(|profile| &profile.id == profile_id)
            }
            endpoint::ClientEndpointId::Local => false,
        });
    }
}

pub(super) fn spawn_worker(
    endpoint_id: endpoint::ClientEndpointId,
    mut reader: PaneScreenReader,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
) -> std::sync::mpsc::Sender<String> {
    let (sender, requests) = std::sync::mpsc::channel::<String>();
    let spawned = std::thread::Builder::new()
        .name("activity-reader".into())
        .spawn(move || {
            while let Ok(pane_id) = requests.recv() {
                let text = reader(&pane_id);
                let event = ClientLoopEvent::RemoteAgentScreen {
                    endpoint_id: endpoint_id.clone(),
                    pane_id,
                    text,
                };
                if event_tx.blocking_send(event).is_err() {
                    break;
                }
            }
        });
    if let Err(error) = spawned {
        debug!(%error, "activity reader thread did not start");
    }
    sender
}

/// Reads through the machine's JSON API bridge, started on first use.
fn saved_machine_reader(profile: endpoint::SavedSshEndpoint) -> PaneScreenReader {
    let mut bridge: Option<crate::remote::SavedSshApiBridge> = None;
    let mut unavailable = false;
    Box::new(move |pane_id| {
        if unavailable {
            return None;
        }
        if bridge.is_none() {
            match crate::remote::SavedSshApiBridge::start(
                profile.id.as_str(),
                &profile.target,
                &profile.session,
                true,
                true,
            ) {
                Ok(started) => bridge = Some(started),
                Err(error) => {
                    debug!(%error, machine = %profile.label, "activity reads unavailable");
                    unavailable = true;
                    return None;
                }
            }
        }
        let socket = bridge.as_ref()?.socket_path().to_owned();
        let client = crate::api::client::ApiClient::for_target(
            crate::api::client::ConnectionTarget::SocketPath(socket),
        );
        read_detection_text(&client, pane_id)
    })
}

fn read_detection_text(client: &crate::api::client::ApiClient, pane_id: &str) -> Option<String> {
    let request = crate::api::schema::Request {
        id: format!("oasis-activity:{pane_id}"),
        method: crate::api::schema::Method::PaneRead(crate::api::schema::PaneReadParams {
            pane_id: pane_id.to_owned(),
            source: crate::api::schema::ReadSource::Detection,
            lines: None,
            format: crate::api::schema::ReadFormat::Text,
            strip_ansi: true,
            intent: Default::default(),
        }),
    };
    let value = client
        .request_value_with_timeout(&request, READ_TIMEOUT)
        .ok()?;
    detection_text_from_response(&value)
}

/// `result.read.text` from a `pane.read` response, as herdr 0.9 sends it.
pub(super) fn detection_text_from_response(value: &serde_json::Value) -> Option<String> {
    value
        .get("result")?
        .get("read")?
        .get("text")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_from_a_herdr_pane_read_response() {
        let response = serde_json::json!({
            "id": "oasis-activity:wA:pF",
            "result": {
                "type": "pane_read",
                "read": {
                    "pane_id": "wA:pF",
                    "source": "detection",
                    "format": "text",
                    "text": "❯ \n───\n  ⏵⏵ auto mode on · 1 shell, 1 monitor",
                    "revision": 0,
                    "truncated": false
                }
            }
        });
        assert!(detection_text_from_response(&response)
            .is_some_and(|text| text.ends_with("1 shell, 1 monitor")));
        let error = serde_json::json!({
            "id": "x",
            "error": {"code": "unknown_method", "message": "nope"}
        });
        assert_eq!(detection_text_from_response(&error), None);
    }

    #[test]
    fn a_worker_answers_each_read_and_blocks_while_idle() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (event_tx, mut events) = tokio::sync::mpsc::channel(8);
        let endpoint_id = endpoint::ClientEndpointId::Ssh(
            endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        );
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let worker = spawn_worker(
            endpoint_id.clone(),
            Box::new(move |pane_id| {
                worker_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                (pane_id == "w1:p1").then(|| "screen".to_owned())
            }),
            event_tx,
        );
        worker.send("w1:p1".into()).unwrap();
        worker.send("w1:p2".into()).unwrap();
        let mut answers = Vec::new();
        runtime.block_on(async {
            for _ in 0..2 {
                if let Some(ClientLoopEvent::RemoteAgentScreen { pane_id, text, .. }) =
                    events.recv().await
                {
                    answers.push((pane_id, text));
                }
            }
        });
        assert_eq!(
            answers,
            vec![
                ("w1:p1".to_owned(), Some("screen".to_owned())),
                ("w1:p2".to_owned(), None),
            ]
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        drop(worker);
    }
}
