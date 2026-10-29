//! Stable endpoint compatibility contract for client-owned shells.
//!
//! The endpoint generation is intentionally independent from the private
//! binary protocol used by same-install CLI, direct-terminal, and handoff
//! paths. Generation 1 is the compatibility floor for Local, SSH, and Cloud
//! shell endpoints and must remain available indefinitely unless retired for a
//! security reason. New JSON fields must be optional or have serde defaults;
//! new enum values need an `Unknown` fallback. Unknown named controls are
//! optional and ignored unless negotiated as part of the core.

use serde::{Deserialize, Serialize};

use super::{ClientShellSnapshot, ClientSurfaceSize, ServerMessage};

pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;
pub const ENDPOINT_HELLO_KIND: &str = "endpoint.hello.v1";
pub const ENDPOINT_WELCOME_KIND: &str = "endpoint.welcome.v1";
pub const SNAPSHOT_CODEC_V1: &str = "shell.snapshot.v1";
pub const ENDPOINT_SNAPSHOT_KIND: &str = SNAPSHOT_CODEC_V1;
pub const SURFACE_CODEC_V1: &str = "shell.surface.v1";
pub const INPUT_CODEC_V1: &str = "shell.input.semantic.v1";
pub const BLOB_CODEC_V1: &str = "shell.blob.v1";
pub const SURFACE_INTEREST_CAPABILITY: &str = "surface_interest";
pub const PRESENTATION_EFFECTS_FENCE_CAPABILITY: &str = "presentation_effects_fence";
pub const PRESENTATION_EFFECTS_SYNC_KIND: &str = "endpoint.presentation.sync.v1";
pub const PRESENTATION_EFFECTS_READY_KIND: &str = "endpoint.presentation.ready.v1";
pub const HEALTH_CHECK_CAPABILITY: &str = "health_check";
pub const HEALTH_PING_KIND: &str = "endpoint.health.ping.v1";
pub const HEALTH_PONG_KIND: &str = "endpoint.health.pong.v1";
pub const AGENT_VIEW_PROJECTION_CAPABILITY: &str = "agent_view_projection";
pub const AGENT_VIEW_PROJECTION_KIND: &str = "endpoint.agent-view.v1";
pub const AGENT_COMPLETIONS_CAPABILITY: &str = "agent_completions";
pub const AGENT_COMPLETIONS_KIND: &str = "endpoint.agent-completions.v1";
/// The server attaches agents' background activity (shells, monitors,
/// subagents) as reserved `oasis:` agent tokens. Herdr servers lack it, and
/// the client reads that activity itself.
pub const BACKGROUND_ACTIVITY_CAPABILITY: &str = "oasis.background_activity";
/// Typed and pasted input is numbered, so a client can find out after a
/// connection drops which of it the server applied, and send the rest exactly
/// once. Herdr servers lack it, and the client sends input unnumbered.
pub const INPUT_SEQUENCE_CAPABILITY: &str = "oasis.input_sequence";
/// Client to server: numbers the pane or popup input message right after it.
pub const INPUT_SEQUENCE_KIND: &str = "endpoint.input.sequence.v1";
/// Server to client: the token's input up to `seq` is applied.
pub const INPUT_ACK_KIND: &str = "endpoint.input.ack.v1";
/// Client to server, on each new connection: how far did this token's input get?
pub const INPUT_RESUME_KIND: &str = "endpoint.input.resume.v1";
/// Server to client: the answer to [`INPUT_RESUME_KIND`].
pub const INPUT_RESUMED_KIND: &str = "endpoint.input.resumed.v1";
/// The longest input token a server keeps; a client's is 32 hex characters.
pub const MAX_INPUT_TOKEN_BYTES: usize = 64;

/// Numbers the input message that follows. A client picks a random `token` for
/// each machine it talks to and numbers its input 1, 2, 3, ...
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointInputSequence {
    pub token: String,
    pub seq: u64,
}

/// `instance` names the server process: one that restarted has forgotten
/// which input it applied before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointInputAck {
    pub token: String,
    pub seq: u64,
    pub instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointInputResume {
    pub token: String,
}

/// `last_applied` is `None` when this server process applied none of the
/// token's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointInputResumed {
    pub token: String,
    pub instance: String,
    #[serde(default)]
    pub last_applied: Option<u64>,
}

/// A named control message carrying `value` as JSON.
pub fn json_control<T: Serialize>(kind: &str, value: &T) -> Option<(String, String)> {
    serde_json::to_string(value)
        .ok()
        .map(|data| (kind.to_owned(), data))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointAgentCompletions {
    pub boot_id: String,
    pub revision: u64,
    pub completions: std::collections::BTreeMap<String, u64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointClientHello {
    pub generation: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub surface_size: ClientSurfaceSize,
    pub pixel_mouse: bool,
    pub direct_graphics: bool,
    pub endpoint_keybindings: bool,
    pub mouse_capture: bool,
    #[serde(default = "default_true")]
    pub surface_active: bool,
    /// Accept the optional cell-retaining surface encoding on this connection.
    #[serde(default)]
    pub surface_reuse: bool,
    /// Accept the optional surface-delta encoding on this connection.
    #[serde(default)]
    pub surface_delta: bool,
    /// Accept the optional scroll-aware patch encoding on this connection.
    #[serde(default)]
    pub surface_scroll: bool,
    #[serde(default)]
    pub snapshot_codecs: Vec<String>,
    #[serde(default)]
    pub surface_codecs: Vec<String>,
    #[serde(default)]
    pub input_codecs: Vec<String>,
    #[serde(default)]
    pub blob_codecs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHandshakeError {
    pub code: String,
    pub message: String,
}

/// Optional companion to a V1 snapshot. The view payload is intentionally opaque to this
/// stable envelope so clients can ignore view language additions they do not understand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointAgentViewProjection {
    pub boot_id: String,
    pub revision: u64,
    #[serde(default)]
    pub view: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointServerWelcome {
    pub generation: u32,
    pub server_version: String,
    pub snapshot_codec: String,
    pub surface_codec: String,
    pub input_codec: String,
    pub blob_codec: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EndpointHandshakeError>,
}

pub fn snapshot_message(snapshot: &ClientShellSnapshot) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: ENDPOINT_SNAPSHOT_KIND.into(),
        data: serde_json::to_string(snapshot)?,
    })
}

pub fn agent_completions_message(
    projection: &EndpointAgentCompletions,
) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: AGENT_COMPLETIONS_KIND.into(),
        data: serde_json::to_string(projection)?,
    })
}

pub fn agent_view_projection_message(
    boot_id: &str,
    revision: u64,
    view: Option<&crate::api::schema::AgentViewSetParams>,
) -> serde_json::Result<ServerMessage> {
    let projection = EndpointAgentViewProjection {
        boot_id: boot_id.to_owned(),
        revision,
        view: view.map(serde_json::to_value).transpose()?,
    };
    Ok(ServerMessage::EndpointControl {
        kind: AGENT_VIEW_PROJECTION_KIND.into(),
        data: serde_json::to_string(&projection)?,
    })
}

impl EndpointClientHello {
    pub fn supports_required_codecs(&self) -> bool {
        self.snapshot_codecs
            .iter()
            .any(|codec| codec == SNAPSHOT_CODEC_V1)
            && self
                .surface_codecs
                .iter()
                .any(|codec| codec == SURFACE_CODEC_V1)
            && self
                .input_codecs
                .iter()
                .any(|codec| codec == INPUT_CODEC_V1)
            && self.blob_codecs.iter().any(|codec| codec == BLOB_CODEC_V1)
    }
}

impl EndpointServerWelcome {
    pub fn compatible(methods: Vec<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods,
            capabilities: vec![
                super::surface_reuse::CAPABILITY.into(),
                super::surface_delta::CAPABILITY.into(),
                super::surface_scroll::CAPABILITY.into(),
                SURFACE_INTEREST_CAPABILITY.into(),
                PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                HEALTH_CHECK_CAPABILITY.into(),
                AGENT_VIEW_PROJECTION_CAPABILITY.into(),
                AGENT_COMPLETIONS_CAPABILITY.into(),
                BACKGROUND_ACTIVITY_CAPABILITY.into(),
                INPUT_SEQUENCE_CAPABILITY.into(),
            ],
            error: None,
        }
    }

    pub fn incompatible(code: &str, message: impl Into<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods: Vec::new(),
            capabilities: Vec::new(),
            error: Some(EndpointHandshakeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> EndpointClientHello {
        EndpointClientHello {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: ClientSurfaceSize { cols: 80, rows: 24 },
            pixel_mouse: true,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
            surface_reuse: false,
            surface_delta: false,
            surface_scroll: false,
            snapshot_codecs: vec![SNAPSHOT_CODEC_V1.into()],
            surface_codecs: vec![SURFACE_CODEC_V1.into()],
            input_codecs: vec![INPUT_CODEC_V1.into()],
            blob_codecs: vec![BLOB_CODEC_V1.into()],
        }
    }

    fn snapshot() -> ClientShellSnapshot {
        ClientShellSnapshot {
            boot_id: "boot".into(),
            revision: 1,
            config_diagnostic: None,
            product_announcement: None,
            update_available: None,
            update_install_command: "oasis update".into(),
            server_keybindings_toml: None,
            latest_release_notes_available: false,
            integration_updates_available: false,
            worktree_directory: String::new(),
            release_notes: None,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: String::new(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            agents: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn hello_ignores_future_named_fields() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value["future_feature"] = serde_json::json!({"enabled": true});
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, hello());
    }

    #[test]
    fn frozen_generation_one_handshake_decodes() {
        let hello: EndpointClientHello = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-hello-v1.json"
        )))
        .unwrap();
        assert!(hello.supports_required_codecs());

        let welcome: EndpointServerWelcome = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-welcome-v1.json"
        )))
        .unwrap();
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert_eq!(welcome.snapshot_codec, SNAPSHOT_CODEC_V1);
        assert_eq!(welcome.surface_codec, SURFACE_CODEC_V1);
        assert_eq!(welcome.input_codec, INPUT_CODEC_V1);
        assert_eq!(welcome.blob_codec, BLOB_CODEC_V1);
        assert!(welcome.capabilities.is_empty());
    }

    #[test]
    fn frozen_generation_one_snapshot_decodes() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-snapshot-v1.json"
        )))
        .unwrap();
        assert_eq!(snapshot.boot_id, "boot-v1");
        assert_eq!(
            snapshot.workspaces[0].agent_status,
            crate::api::schema::AgentStatus::Unknown
        );
    }

    #[test]
    fn snapshot_message_uses_named_json_control() {
        let snapshot = snapshot();
        let ServerMessage::EndpointControl { kind, data } = snapshot_message(&snapshot).unwrap()
        else {
            panic!("snapshot should use endpoint control");
        };
        assert_eq!(kind, ENDPOINT_SNAPSHOT_KIND);
        let decoded: ClientShellSnapshot = serde_json::from_str(&data).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn agent_view_projection_is_an_optional_revision_bound_control() {
        let view = crate::api::schema::AgentViewSetParams {
            source: "example.views".into(),
            label: Some("focus".into()),
            filter: None,
            sort: Vec::new(),
        };
        let ServerMessage::EndpointControl { kind, data } =
            agent_view_projection_message("boot", 7, Some(&view)).unwrap()
        else {
            panic!("projection should use endpoint control");
        };
        assert_eq!(kind, AGENT_VIEW_PROJECTION_KIND);
        let projection: EndpointAgentViewProjection = serde_json::from_str(&data).unwrap();
        assert_eq!(projection.boot_id, "boot");
        assert_eq!(projection.revision, 7);
        assert_eq!(
            projection
                .view
                .map(serde_json::from_value)
                .transpose()
                .unwrap(),
            Some(view)
        );
    }

    #[test]
    fn snapshot_json_tolerates_future_fields_and_command_actions() {
        let mut snapshot = match snapshot_message(&snapshot()).unwrap() {
            ServerMessage::EndpointControl { data, .. } => {
                serde_json::from_str::<serde_json::Value>(&data).unwrap()
            }
            _ => unreachable!(),
        };
        snapshot["future_projection"] = serde_json::json!({"enabled": true});
        snapshot["commands"] = serde_json::json!([{
            "command_id": "future",
            "binding_label": "x",
            "binding_labels": ["x"],
            "action": "FutureAction",
            "description": null
        }]);

        let decoded: ClientShellSnapshot = serde_json::from_value(snapshot).unwrap();
        assert_eq!(
            decoded.commands[0].action,
            crate::protocol::ClientShellCommandAction::Unknown
        );
    }

    #[test]
    fn legacy_hello_defaults_to_an_active_surface() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value.as_object_mut().unwrap().remove("surface_active");
        value.as_object_mut().unwrap().remove("surface_reuse");
        value.as_object_mut().unwrap().remove("surface_delta");
        value.as_object_mut().unwrap().remove("surface_scroll");
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert!(decoded.surface_active);
        assert!(!decoded.surface_reuse);
        assert!(!decoded.surface_delta);
        assert!(!decoded.surface_scroll);
    }

    #[test]
    fn compatible_server_advertises_endpoint_lifecycle_capabilities() {
        let welcome = EndpointServerWelcome::compatible(Vec::new());
        assert_eq!(
            welcome.capabilities,
            vec![
                super::super::surface_reuse::CAPABILITY.to_string(),
                super::super::surface_delta::CAPABILITY.to_string(),
                super::super::surface_scroll::CAPABILITY.to_string(),
                SURFACE_INTEREST_CAPABILITY.to_string(),
                PRESENTATION_EFFECTS_FENCE_CAPABILITY.to_string(),
                HEALTH_CHECK_CAPABILITY.to_string(),
                AGENT_VIEW_PROJECTION_CAPABILITY.to_string(),
                AGENT_COMPLETIONS_CAPABILITY.to_string(),
                BACKGROUND_ACTIVITY_CAPABILITY.to_string(),
                INPUT_SEQUENCE_CAPABILITY.to_string(),
            ]
        );
    }

    #[test]
    fn input_sequence_controls_round_trip_as_json() {
        let ack = EndpointInputAck {
            token: "t".into(),
            seq: 7,
            instance: "i".into(),
        };
        let (kind, data) = json_control(INPUT_ACK_KIND, &ack).unwrap();
        assert_eq!(kind, INPUT_ACK_KIND);
        assert_eq!(
            serde_json::from_str::<EndpointInputAck>(&data).unwrap(),
            ack
        );
        let resumed: EndpointInputResumed =
            serde_json::from_str(r#"{"token":"t","instance":"i"}"#).unwrap();
        assert_eq!(
            resumed.last_applied, None,
            "a missing count means none applied"
        );
        let sequence = EndpointInputSequence {
            token: "t".into(),
            seq: 1,
        };
        let (_, data) = json_control(INPUT_SEQUENCE_KIND, &sequence).unwrap();
        assert_eq!(
            serde_json::from_str::<EndpointInputSequence>(&data).unwrap(),
            sequence
        );
    }

    #[test]
    fn required_codecs_are_explicit() {
        let mut value = hello();
        assert!(value.supports_required_codecs());
        value.snapshot_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.surface_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.input_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.blob_codecs.clear();
        assert!(!value.supports_required_codecs());
    }

    #[test]
    fn welcome_ignores_future_named_fields() {
        let welcome = EndpointServerWelcome::compatible(vec!["pane.close".into()]);
        let mut value = serde_json::to_value(&welcome).unwrap();
        value["future_service"] = serde_json::json!("v2");
        let decoded: EndpointServerWelcome = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, welcome);
    }
}
