# Parity checklist: oasis TUI shell vs clawmux

Generated from `src/client/shell/*` on 2026-10-23. Tick items as the GUI matches them.


**How the TUI works, in short:** keys go through `input.rs:623 route_key_press`. Each mode (Terminal, Prefix, Navigate, Resize, Copy) handles its own keys, and a matched keybind goes to `actions.rs:3 record_binding`. That either opens an overlay locally or builds a server method (`actions.rs:~895 endpoint_method_for_action`) and sends it with `push_endpoint_method_with_kind` (`actions.rs:315`). If the server lacks a method, the client shows an "Action unavailable" notice instead (`actions.rs:332`).

## Sidebar (spaces / workspaces)
- [ ] Expanded sidebar with " spaces" header, workspace rows, scrollbar, status footer. [C] `src/client/shell/sidebar.rs:258`
- [ ] Footer: "🌴 oasis" mark, "leader: <prefix>" hint, "new" button, "menu" launcher (shows a ● when there's an update), collapse toggle. [C] `sidebar.rs:42`, `sidebar.rs:452-505`
- [ ] Collapsed sidebar (numbered compact rows; style set by `sidebar_collapsed_mode`). [C] `sidebar.rs:106`
- [ ] Workspace row tokens: state icon/text, workspace label, branch, git ahead/behind, custom tokens with style rules. [C] `sidebar.rs:~730`, `src/config/sidebar.rs:134` (SpaceSidebarToken), `src/ui/sidebar/tokens.rs`
- [ ] Workspace status shown is a roll-up of its agents' states. [C] `sidebar.rs:~699 displayed_workspace_status`
- [ ] Worktree grouping: child worktrees drawn as a tree (├─ └─) under the parent repo; group collapse toggle ▸/▾. [C][P] `sidebar.rs:524 workspace_entries`, `sidebar.rs:~669 render_parent_group_toggle`, click at `mouse.rs:~2215`
- [ ] Click a workspace row to focus it: `workspace.focus`. `endpoint_navigation.rs:33 finish_endpoint_workspace_press`
- [ ] Drag a workspace row to reorder: `workspace.move`; dragging a group parent uses `workspace.move_block`. `mouse.rs:758`, `mouse.rs:773`
- [ ] Drag the sidebar edge to resize; double-click the edge resets to the configured width. [C][P] `mouse.rs:2077-2097`
- [ ] Drag the divider between the spaces and agents sections to change the split. [C][P] `mouse.rs:2097`, `src/ui/sidebar.rs:50,68`
- [ ] Toggle sidebar collapse with the button or keybind. [C][P] `mouse.rs:2068`, `actions.rs:13`
- [ ] Mouse wheel and scrollbar drag in the workspace list. [C] `mouse.rs:2039-2050`, `mouse.rs:1146`; helpers in `scroll.rs:5,56,79`
- [ ] Brief highlight of a pending workspace after a keyboard jump. [C] `workspace_navigation.rs:26,38`
- [ ] Right-click a workspace opens its context menu (see Overlays). `mouse.rs:1966`

## Agents panel
- [ ] " agents" section under spaces: a row per agent pane, with header and sort label ("grouped" or "priority"). [C] `agent_sidebar.rs:52,90,154`
- [ ] Agent row tokens: state icon/text, machine, workspace, tab, pane, agent, terminal title (raw or stripped), background work (shells, monitors, subagents), next wakeup, custom tokens. [C] `src/config/sidebar.rs:108` (AgentSidebarToken), `agent_sidebar.rs:248,321`
- [ ] Click the sort toggle to switch between Spaces and Priority order. [C][P] `mouse.rs:~2149`
- [ ] Click an agent to focus its pane: `pane.focus` (on a remote machine it activates the endpoint first). `mouse.rs:~2279`, `endpoint_navigation.rs:84`
- [ ] Wheel and scrollbar drag in the agent list. [C] `mouse.rs:2022-2035`, `mouse.rs:1162`
- [ ] Combined agent list across all connected machines. [C] `aggregate_navigation.rs:56,263`, `endpoint_agents.rs:4,45`
- [ ] Previous/next/focus-agent-N keybinds: `pane.focus`. `actions.rs:911-950`, order from `agent_sidebar.rs:20`
- [ ] Viewing a surface marks its agents as seen, which clears the "done" state. `endpoints.rs:661` (server side is `agent.view.set`, called outside the shell module)
- [ ] Background screen reads for remote agents. `remote_activity.rs:205-244`

## Tabs
- [ ] Tab bar: tab labels (a zoomed tab gets a " Z" suffix), │ separators, "<" ">" scroll buttons, "…" for clipped tabs, "+" new-tab button, status area. [C] `tabs.rs:7`, `tabs.rs:257,374`
- [ ] Tab bar position (`tab_bar_position`), and hiding it when there's only one tab. [C] config at `state.rs:22-23`
- [ ] Click a tab to focus it: `tab.focus`. `mouse.rs:1491`
- [ ] Drag a tab to reorder, with a drop indicator: `tab.move`. `mouse.rs:1360,1398`, `tabs.rs:294`
- [ ] The "+" button runs NewTab: `tab.create`, or a name prompt if `prompt_new_tab_name` is set. `mouse.rs:~2183`, `actions.rs:1039`, `actions.rs:134`
- [ ] Scroll buttons, and keeping the active tab in view. [C] `mouse.rs:~2189-2197`, `tabs.rs:330,355`
- [ ] Right-click a tab: menu with New tab, Rename, Close. `context_menu.rs:48,282`

## Panes
- [ ] Pane content is drawn from server cell grids, with patches applied on top. [C] `composition.rs:150`, `surface_patch.rs`
- [ ] Click a pane to focus it: `pane.focus`. `mouse.rs:~2420`
- [ ] Mouse events pass through to apps that report the mouse (left, middle, drag, move, wheel). `mouse.rs:2512 push_pane_mouse_event`, `mouse.rs:2427`
- [ ] Wheel scrollback when the app doesn't capture the mouse: `pane.scroll`; step set by `mouse_scroll_lines`. `mouse.rs:53,83`
- [ ] Pane scrollbar drag. `mouse.rs:2312`
- [ ] Drag a split divider to resize: `layout.set_split_ratio`. `mouse.rs:2342,1295,1461`
- [ ] Split vertical/horizontal: `pane.split`. `actions.rs:1066`
- [ ] Close pane, with confirmation when `confirm_close` is set: `pane.close`. `overlay_input.rs:1104`
- [ ] Zoom: `pane.zoom`. Swap: `pane.swap`. Focus by direction: `pane.focus_direction`. Cycle or last pane: `pane.focus`. `actions.rs:1048-1117`
- [ ] Resize: `pane.resize` (resize mode, or the direct resize keybinds). `actions.rs:1127`, `input.rs:1056`
- [ ] Rename pane or clear its name: `pane.rename`. `overlay_input.rs:1064`, `context_menu.rs:388`
- [ ] Clear pane: `pane.clear`. Edit scrollback in $EDITOR: `pane.edit_scrollback`. `actions.rs:1121,1124`
- [ ] Per-pane "send right-clicks to pane" toggle: `pane.input.set`. `context_menu.rs:440`; right-click routing at `mouse.rs:1909`
- [ ] Popup panes from custom commands; input goes to the popup while it is open. `input.rs:1183`, `actions.rs:243`

## Keybinds (prefix `ctrl+b`: `src/config/keybinds.rs:14`, defaults at `src/config/model.rs:1124-1185`; struct at `keybinds.rs:352`)
Pressing the prefix twice sends a literal prefix key to the pane (`input.rs:707`). Esc leaves prefix mode.
- [ ] help `prefix+?` (help overlay [C]) · settings `prefix+s` (settings overlay [C])
- [ ] new_workspace `prefix+shift+n`: `workspace.create`, or a name prompt
- [ ] new_worktree `prefix+shift+g`: `worktree.list`, then the create overlay, then `worktree.create`
- [ ] open_worktree (unbound): open overlay, then `worktree.open` · remove_worktree (unbound): `worktree.remove`
- [ ] rename_workspace `prefix+shift+w`: `workspace.rename` · close_workspace `prefix+shift+d`: `workspace.close`
- [ ] workspace_picker `prefix+w`: Navigate mode [C] · goto `prefix+g`: session navigator overlay [C]
- [ ] detach `prefix+q` (client detaches) · reload_config `prefix+shift+r`: `server.reload_config` plus a local config reload
- [ ] open_notification_target `prefix+o`: `pane.focus` on the visible toast's pane
- [ ] previous_workspace / next_workspace (unbound): `workspace.focus`; these cycle across machines when several are connected (`endpoint_navigation.rs:106`)
- [ ] previous_agent / next_agent (unbound), focus_agent 1-9 (unbound; `keys.indexed.agents`): `pane.focus`
- [ ] new_tab `prefix+c`: `tab.create` · rename_tab `prefix+shift+t`: `tab.rename` · close_tab `prefix+shift+x`: `tab.close`
- [ ] previous_tab `prefix+p` / next_tab `prefix+n` / switch_tab `prefix+1..9`: `tab.focus`
- [ ] move_tab_previous / move_tab_next (unbound): `tab.move`
- [ ] switch_workspace 1-9 (unbound; `keys.indexed.workspaces`): `workspace.focus`
- [ ] rename_pane `prefix+shift+p` · edit_scrollback `prefix+e` · clear_pane (unbound) · copy_mode `prefix+[`
- [ ] focus_pane_left/down/up/right `prefix+h/j/k/l`: `pane.focus_direction`
- [ ] swap_pane_left/down/up/right `prefix+shift+h/j/k/l`: `pane.swap`
- [ ] cycle_pane_next `prefix+tab` / cycle_pane_previous `prefix+shift+tab` / last_pane (unbound): `pane.focus`
- [ ] split_vertical `prefix+v` / split_horizontal `prefix+minus`: `pane.split`
- [ ] close_pane `prefix+x` · zoom `prefix+z` · resize_mode `prefix+r`
- [ ] resize_pane_left/down/up/right (unbound): `pane.resize`
- [ ] toggle_sidebar `prefix+b` [C][P]
- [ ] Custom commands `[[keys.command]]` (prefix or direct chords): `command.invoke`. A plugin action also sends the current selection; a popup action opens a popup. `actions.rs:180-260`
- [ ] Direct (no-prefix) chords are checked before the prefix in Terminal mode. `input.rs:686`
- [ ] Navigate mode keys: up/down select a workspace (`navigate_workspace_up/down`); h/j/k/l and left/right move focus; tab/shift+tab cycle panes; 1..9 pick a workspace; enter opens it; esc, ctrl+[ or the prefix cancels. `input.rs:765-950`, `model.rs:1137-1142`
- [ ] Resize mode keys: h/j/k/l or arrows resize; esc or enter exit. `input.rs:1056-1080`
- [ ] remote_image_paste `ctrl+v`: sends the local clipboard image to a remote session. `model.rs:1151`, `input.rs:1108`
- [ ] Mode bar badges: PREFIX, NAVIGATE, RESIZE, COPY, ERROR. [C] `render.rs:31-180`
- [ ] Keybind conflict and reserved-key diagnostics. `keybinds.rs:1045-1111`

## Mouse (also see the Sidebar, Tabs and Panes sections)
- [ ] Event routing order: overlays, then chrome controls, then panes. Respects `mouse_capture`. `mouse.rs:786`
- [ ] Right-click opens context menus on workspaces, tabs and panes; a pane can be set to receive right-clicks instead (`right_click_passthrough_modifiers`). `mouse.rs:1909-1990`
- [ ] Global "menu" launcher click. `mouse.rs:~2168`, `mouse.rs:1517`
- [ ] Hovering a link underlines it and resolves it: `pane.link.resolve`. `link_hover.rs:175,282,446`
- [ ] Clicking a link (plain click in an already-focused pane, held for the double-click window; `click_opens_links`): `pane.link.activate`, then the client opens it as a safe web URL or local path. `mouse.rs:362,429`, `actions.rs:713,722`
- [ ] Mobile/narrow layout: header, tab status, switcher list, close button, own mouse handling (below `mobile_width_threshold`). [C] `mobile.rs:50,363,944`, `state.rs:1239`

## Copy and selection
- [ ] Drag to select inside a pane (when the app doesn't grab the mouse), with autoscroll at the edges. [C] `mouse.rs:1854-1871`, `mouse.rs:451`
- [ ] Double-click selects a word, and dragging extends by word: `pane.selection.read`. `word_selection.rs:21,62,93`
- [ ] Copy-on-select (`copy_on_select`), or ctrl/cmd+c copies a visible selection: `pane.selection.read`, then a local clipboard write (OSC52). `input.rs:660`, `actions.rs:265,644`
- [ ] Clipboard toast (`clipboard_toast_enabled` and position). [C] `state.rs:34-35`
- [ ] Copy mode (`prefix+[`): q quit; y yank; v or space select; V line select; h/j/k/l and arrows; PgUp/PgDn, ctrl+b/f/u/d; Home/End; g/G; 0 $ ^; w b e W B E; { }. `copy_mode.rs:104-264`
  - Motions: `pane.copy_motion` (`copy_mode.rs:726`). Search with / ? n N: `pane.copy_search` (`copy_mode.rs:759`). Enter: `copy_mode.rs:12`. Exit: `copy_mode.rs:813`.
- [ ] Modal paste into overlay text fields. `input.rs:562,607`

## Overlays and dialogs (enum at `state.rs:632`; rendered by `overlays.rs:36`)
- [ ] Rename / new-name prompt for new workspace, workspace, new tab, tab, pane; text editor with save, ctrl+c clear, esc cancel buttons. Sends `workspace.create`, `workspace.rename`, `tab.create`, `tab.rename`, `pane.rename`. `overlays.rs:756`, `overlay_input.rs:1013-1064`, `text_editor.rs`
- [ ] Close confirmation (↵ confirm / esc cancel) for workspace, group, tab, pane. `overlays.rs:1368`, `overlay_input.rs:1160,1205`
- [ ] Help overlay: grouped keybind list (global, navigation, workspaces/tabs, panes, custom) with search and a scrollbar. [C] `overlays.rs:1165,1229`, `src/input/keybind_help.rs:75`
- [ ] Session navigator "Go to" (`prefix+g`): tree of machines, workspaces and agents/terminals, search, filters a/b/w/i/d (all/blocked/working/idle/done), ←→ jump workspace, enter opens. Sends `workspace.focus`, `tab.focus`, `pane.focus`. `overlays.rs:816-1144`, `overlay_input.rs:251-334`, `aggregate_navigation.rs:278`
- [ ] Worktree create (branch and path preview), open (list), remove (with force retry). Sends `worktree.list`, `worktree.create`, `worktree.open`, `worktree.remove`. `worktree_overlays.rs:3,109,308`, `worktrees.rs:178-368`
- [ ] Context menus. `context_menu.rs:4`, activation at `context_menu.rs:222,282,358`
  - Workspace: Rename, Close (or Close group), New worktree, Open worktree…, Delete worktree checkout…, Expand/Collapse.
  - Tab: New tab, Rename, Close.
  - Pane: Rename pane, Clear pane name, Swap with focused pane, Split right/down, Zoom, right-click passthrough toggle, Close pane.
- [ ] Global menu: settings, keybinds, reload config, "update ready"/"what's new", detach; badges for an update or integration updates. `global_menu.rs:22,80`, `overlays.rs:87`
- [ ] Release notes (scrollable): `release_notes.dismiss`. `overlays.rs:309`, `overlay_input.rs:61-93`
- [ ] Product announcement: `product_announcement.dismiss`. `overlays.rs:425`, `overlay_input.rs:4`
- [ ] Onboarding (first run) and herdr-import offer. Mostly [C]. `overlays.rs:543,652`, `overlay_input.rs:179-235`

## Notifications
- [ ] Agent-state toasts arrive from the server as semantic notifications, wait `toast_delay_seconds`, and queue so only one shows at a time. `notification_policy.rs:99,147,40,56`
- [ ] Delivery modes: off, in-app card (with `toast_position`), terminal OSC, or system notification; sounds on state change (`sound_enabled`). `state.rs:767` (effects), `notifications.rs:107,180`
- [ ] Click the toast or `prefix+o` to jump to its pane: `pane.focus`, switching machine if needed. `notification_policy.rs:65`
- [ ] Endpoint notices (unsupported method, rejected, timeout, unavailable) shown for 3 to 8 seconds. [C] `actions.rs:276`, `endpoint_notices.rs:56`
- [ ] Mobile banners for notices and notifications. [C] `notifications.rs:10,67`, `endpoint_notices.rs:4`

## Machines (SSH endpoints)
- [ ] " machines" sidebar section: each machine row has a ▸/▾ collapse marker and status (● online, ◐ connecting/reconnecting, ! attention/auth/error, · disabled), with that machine's workspaces nested under it. [C] `endpoint_sidebar.rs:237,597`, `endpoints.rs:724`
- [ ] Click a machine: switch to it (client-side ActivateEndpoint action) or collapse it; clicking an unready machine retries the connection now. `endpoint_navigation.rs:45,232`, `machine_diagnostics.rs:87`
- [ ] Machine diagnostic badges and hints (`oasis machine reconnect|status <id>`). `machine_diagnostics.rs:127,173`
- [ ] Lifecycle banner `<symbol> <label> · <state>`. `endpoint_notices.rs:22`
- [ ] Each machine has its own snapshot cache, collapsed groups and preferences. `endpoints.rs:519-704`, `preferences.rs:33,45`
- [ ] Footer "new · <machine>" shows which machine a new workspace goes to. `endpoint_sidebar.rs:523`
- [ ] Workspace, tab and pane actions are refused when the active machine is offline. `actions.rs:327`

## Settings (`prefix+s`; `settings.rs`, `settings_overlay.rs:36`)
- [ ] Sections: theme, indicators, sound, toast, integrations. `state.rs:398`
- [ ] Theme: live preview; esc restores the previous theme. `settings.rs:142,154`
- [ ] Indicators: dots or symbols. Sound: on or off. Notification popups: off, inside herdr, via terminal, via system. `settings_overlay.rs:165-191`
- [ ] Saving writes the config file locally (`config::write_edit`), then sends `server.reload_config`. `settings.rs:162`
- [ ] Agent integrations list (installed, update available, available, not found) and install recommended: `integration.list`, `integration.install`. `settings.rs:229,247`, `settings_overlay.rs:298`

## Misc
- [ ] Config reload: `server.reload_config` plus a client reload; startup config diagnostics. `actions.rs:68`, `config.rs`
- [ ] Saved chrome preferences: sidebar width, collapsed state, section split, agent sort, collapsed groups. [P] `config.rs:29`, `preferences.rs`
- [ ] Prompting before new tab/workspace names (`prompt_new_*_name`) and `confirm_close`. `state.rs:42-44`
- [ ] Switch the OS input method to ASCII while in prefix mode (macOS/Windows). `input_source.rs:25`
- [ ] Inline graphics (kitty/sixel) composed over panes. `graphics.rs:59`
- [ ] Redraw on focus gained, and following the host terminal's light/dark theme. `input.rs:40,75`

The checklist only covers what `src/client/shell` does itself. It doesn't cover the code that runs `ActivateEndpoint`, the clipboard write and safe-URL opening (they live outside the shell module), or the commands for adding and removing SSH machines.