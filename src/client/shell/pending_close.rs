//! Closes shown before the server confirms them.
//!
//! A close request can take a round trip and more: a server waits for the
//! closed pane's processes, and Herdr servers do so on the thread that sends
//! updates. So the window hides the target at once and keeps hiding it in
//! every snapshot until the server's answer settles it. A failed or timed-out
//! close puts the target back with the usual endpoint notice.

use super::*;
use crate::client::model::{hide_closed_target, CloseTarget};

pub(super) struct PendingClose {
    pub(super) request_id: String,
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) boot_id: String,
    pub(super) target: CloseTarget,
    /// The server answered success; the next snapshot is the truth.
    pub(super) confirmed: bool,
}

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
        self.pending_closes.push(PendingClose {
            request_id,
            endpoint_id: endpoint_id.clone(),
            boot_id,
            target,
            confirmed: false,
        });
        self.reapply_pending_closes(&endpoint_id);
    }

    /// Applies the server's answer to a close. Returns whether the view changed.
    pub(super) fn settle_pending_close(&mut self, request_id: &str, succeeded: bool) -> bool {
        let Some(index) = self
            .pending_closes
            .iter()
            .position(|pending| pending.request_id == request_id)
        else {
            return false;
        };
        let endpoint_id = self.pending_closes[index].endpoint_id.clone();
        if succeeded {
            let still_present = self
                .unfiltered_snapshots
                .get(&endpoint_id)
                .is_some_and(|raw| self.pending_closes[index].target.present_in(raw));
            if still_present {
                self.pending_closes[index].confirmed = true;
                return false;
            }
        }
        self.pending_closes.remove(index);
        self.reapply_pending_closes(&endpoint_id);
        !succeeded
    }

    /// Keeps hiding unsettled closes in a snapshot just received, and settles
    /// the ones it shows are done. `snapshot` must be what the server sent.
    pub(super) fn filter_incoming_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: &mut ClientShellSnapshot,
    ) {
        if !self
            .pending_closes
            .iter()
            .any(|pending| &pending.endpoint_id == endpoint_id)
        {
            return;
        }
        self.pending_closes.retain(|pending| {
            &pending.endpoint_id != endpoint_id
                || (pending.boot_id == snapshot.boot_id
                    && !pending.confirmed
                    && pending.target.present_in(snapshot))
        });
        if self
            .pending_closes
            .iter()
            .any(|pending| &pending.endpoint_id == endpoint_id)
        {
            self.unfiltered_snapshots
                .insert(endpoint_id.clone(), Box::new(snapshot.clone()));
            for pending in &self.pending_closes {
                if &pending.endpoint_id == endpoint_id {
                    hide_closed_target(snapshot, &pending.target);
                }
            }
        } else {
            self.unfiltered_snapshots.remove(endpoint_id);
        }
    }

    /// Puts back every close on the active endpoint, e.g. when its requests
    /// are abandoned on a reset; later snapshots show what really happened.
    pub(super) fn abandon_pending_closes(&mut self) {
        let endpoint_id = self.active_endpoint_id.clone();
        if !self
            .pending_closes
            .iter()
            .any(|pending| pending.endpoint_id == endpoint_id)
        {
            return;
        }
        self.pending_closes
            .retain(|pending| pending.endpoint_id != endpoint_id);
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
        let mut any = false;
        for pending in &self.pending_closes {
            if &pending.endpoint_id == endpoint_id {
                hide_closed_target(&mut shown, &pending.target);
                any = true;
            }
        }
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
