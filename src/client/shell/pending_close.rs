//! Closes shown before the server confirms them.
//!
//! A close request can take a round trip and more: a server waits for the
//! closed pane's processes, and Herdr servers do so on the thread that sends
//! updates. So the window hides the target at once and keeps hiding it in
//! every snapshot until the server's answer settles it. A failed or timed-out
//! close puts the target back with the usual endpoint notice.

use super::*;
use crate::client::model::{CloseSettle, CloseTarget, IncomingCloses, PendingCloses};

/// The window's pending closes: per endpoint, keyed by request id.
pub(super) type ShellPendingCloses = PendingCloses<ClientEndpointId, String>;

impl ClientShellState {
    /// What a close method removes, if `method` is a close.
    pub(super) fn close_target_for(
        &self,
        method: &crate::api::schema::Method,
    ) -> Option<CloseTarget> {
        use crate::api::schema::Method;
        match method {
            Method::TabClose(target) => Some(CloseTarget::Tab(target.tab_id.clone())),
            Method::PaneClose(target) => Some(CloseTarget::Pane(target.pane_id.clone())),
            Method::WorkspaceClose(params) => {
                let snapshot = self.snapshot.as_deref()?;
                let workspace = snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == params.workspace_id)?;
                let group_key = workspace
                    .worktree
                    .as_ref()
                    .filter(|_| params.close_group)
                    .map(|worktree| worktree.key.clone());
                let ids = match group_key {
                    Some(key) => snapshot
                        .workspaces
                        .iter()
                        .filter(|member| {
                            member
                                .worktree
                                .as_ref()
                                .is_some_and(|worktree| worktree.key == key)
                        })
                        .map(|member| member.workspace_id.clone())
                        .collect(),
                    None => vec![params.workspace_id.clone()],
                };
                Some(CloseTarget::Workspaces(ids))
            }
            _ => None,
        }
    }

    /// Hides `target` on the active endpoint until `request_id` settles.
    pub(super) fn begin_pending_close(&mut self, request_id: String, target: CloseTarget) {
        let endpoint_id = self.active_endpoint_id.clone();
        let Some(cached) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.clone())
        else {
            return;
        };
        let boot_id = cached.boot_id.clone();
        self.unfiltered_snapshots
            .entry(endpoint_id.clone())
            .or_insert(cached);
        self.pending_closes
            .begin(request_id, endpoint_id.clone(), boot_id, target);
        self.reapply_pending_closes(&endpoint_id);
    }

    /// Applies the server's answer to a close. Returns whether the view changed.
    pub(super) fn settle_pending_close(&mut self, request_id: &str, succeeded: bool) -> bool {
        let unfiltered = &self.unfiltered_snapshots;
        let request_id = request_id.to_owned();
        match self
            .pending_closes
            .settle(&request_id, succeeded, |endpoint| {
                unfiltered.get(endpoint).map(Box::as_ref)
            }) {
            CloseSettle::Unknown | CloseSettle::Confirmed => false,
            CloseSettle::Settled { endpoint, failed } => {
                self.reapply_pending_closes(&endpoint);
                failed
            }
        }
    }

    /// Keeps hiding unsettled closes in a snapshot just received, and settles
    /// the ones it shows are done. `snapshot` must be what the server sent.
    pub(super) fn filter_incoming_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: &mut ClientShellSnapshot,
    ) {
        match self.pending_closes.filter_incoming(endpoint_id, snapshot) {
            IncomingCloses::Untracked => {}
            IncomingCloses::AllSettled => {
                self.unfiltered_snapshots.remove(endpoint_id);
            }
            IncomingCloses::StillHidden(raw) => {
                self.unfiltered_snapshots.insert(endpoint_id.clone(), raw);
            }
        }
    }

    /// Puts back every close on the active endpoint, e.g. when its requests
    /// are abandoned on a reset; later snapshots show what really happened.
    pub(super) fn abandon_pending_closes(&mut self) {
        let endpoint_id = self.active_endpoint_id.clone();
        if !self.pending_closes.abandon(&endpoint_id) {
            return;
        }
        if let Some(raw) = self.unfiltered_snapshots.remove(&endpoint_id) {
            if let Some(endpoint) = self
                .endpoints
                .iter_mut()
                .find(|endpoint| endpoint.endpoint_id == endpoint_id)
            {
                endpoint.snapshot = Some(raw);
            }
        }
    }

    /// Rebuilds the endpoint's shown snapshot from the server's last one,
    /// hiding whatever is still pending.
    fn reapply_pending_closes(&mut self, endpoint_id: &ClientEndpointId) {
        let Some(raw) = self.unfiltered_snapshots.get(endpoint_id) else {
            return;
        };
        let mut shown = raw.clone();
        let any = self.pending_closes.apply(endpoint_id, &mut shown);
        if !any {
            self.unfiltered_snapshots.remove(endpoint_id);
        }
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        let generation = endpoint.snapshot_generation;
        endpoint.snapshot = Some(shown.clone());
        if endpoint_id == &self.active_endpoint_id {
            self.apply_active_snapshot(shown, generation);
        }
    }
}
