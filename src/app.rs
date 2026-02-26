use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::selection::SelectionType;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::draw::DrawContext;
use crate::dropdown::{DropdownElement, DropdownMenu, MenuAction, MenuEntry};
use crate::gpu::GpuContext;
use crate::icons;
use crate::icons::IconManager;
use crate::layout::Rect;
use crate::saved_sessions::{now_unix, SavedAuthType, SavedSession, SavedSessions};
use crate::ssh_dialog::{AuthMethod, SshDialogWindow, SshPrefill, SshResult};
use crate::tab_bar::{TabBar, TabBarElement};
use crate::terminal_panel::{EventProxy, PanelId, TermSize, TerminalEvent, TerminalPanel};
use crate::theme::Theme;

pub struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    icon_manager: IconManager,
    event_proxy_raw: EventLoopProxy<TerminalEvent>,
    theme: Theme,
    tabs: Vec<TerminalPanel>,
    active_tab: usize,
    tab_bar: TabBar,
    dropdown: DropdownMenu,
    ssh_dialog_window: Option<SshDialogWindow>,
    saved_sessions: SavedSessions,
    cursor_position: (f32, f32),
    super_pressed: bool,
    ctrl_pressed: bool,
    alt_pressed: bool,
    mouse_left_pressed: bool,
    last_click_time: Instant,
    click_count: u8,
    screenshot_pending: Option<String>,
}

impl App {
    pub fn new(event_proxy_raw: EventLoopProxy<TerminalEvent>) -> Self {
        let theme = Theme::new();
        let saved_sessions = SavedSessions::load();
        Self {
            window: None,
            gpu: None,
            icon_manager: IconManager::new(),
            event_proxy_raw,
            theme,
            tabs: Vec::new(),
            active_tab: 0,
            tab_bar: TabBar::new(),
            dropdown: DropdownMenu::new(),
            ssh_dialog_window: None,
            saved_sessions,
            cursor_position: (0.0, 0.0),
            super_pressed: false,
            ctrl_pressed: false,
            alt_pressed: false,
            mouse_left_pressed: false,
            last_click_time: Instant::now(),
            click_count: 0,
            screenshot_pending: std::env::var("SCREENSHOT").ok().filter(|s| !s.is_empty()),
        }
    }

    /// Compute shared viewport params for a new panel.
    fn new_panel_params(
        &self,
        gpu: &GpuContext,
    ) -> (PanelId, crate::terminal_panel::PanelViewport, EventProxy) {
        let panel_id = PanelId::next();
        let event_proxy = EventProxy::new(self.event_proxy_raw.clone(), panel_id);
        let scale = gpu.scale_factor;
        let area = self.panel_area(gpu);
        let tab_h = TabBar::height(&self.theme.tab_bar, scale);
        let vp = TerminalPanel::compute_viewport(&area, &gpu.cell, scale, tab_h, &self.theme.panel);
        (panel_id, vp, event_proxy)
    }

    fn create_terminal_panel(
        &self,
        gpu: &GpuContext,
        shell: Option<String>,
        args: Vec<String>,
    ) -> TerminalPanel {
        let (panel_id, vp, event_proxy) = self.new_panel_params(gpu);
        let cell_px = (
            (gpu.cell.width * gpu.scale_factor) as u16,
            (gpu.cell.height * gpu.scale_factor) as u16,
        );
        TerminalPanel::new(panel_id, TermSize::new(vp.cols, vp.rows), cell_px, event_proxy, shell, args)
    }

    fn panel_area(&self, gpu: &GpuContext) -> Rect {
        let scale = gpu.scale_factor;
        let pad = self.theme.general.panel_area_padding * scale;
        Rect {
            x: pad,
            y: pad,
            width: gpu.surface_config.width as f32 - 2.0 * pad,
            height: gpu.surface_config.height as f32 - 2.0 * pad,
        }
    }

    fn update_viewports(&mut self) {
        let gpu = self.gpu.as_ref().unwrap();
        let cell = gpu.cell;
        let scale = gpu.scale_factor;
        let area = self.panel_area(gpu);
        let tab_h = TabBar::height(&self.theme.tab_bar, scale);

        if let Some(panel) = self.tabs.get_mut(self.active_tab) {
            let viewport = TerminalPanel::compute_viewport(&area, &cell, scale, tab_h, &self.theme.panel);
            panel.set_viewport(viewport, &cell);
        }
    }

    fn update_tab_bar(&mut self) {
        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let pad = self.theme.general.panel_area_padding * scale;
        let panel_width = gpu.surface_config.width as f32 - 2.0 * pad;

        let titles: Vec<String> = self
            .tabs
            .iter()
            .map(|p| p.title().to_string())
            .collect();

        self.tab_bar.update(
            &titles,
            self.active_tab,
            panel_width,
            pad,
            scale,
            &mut gpu.font_system,
            &self.theme.tab_bar,
        );
    }

    /// Returns true if a screenshot was captured (signals auto-exit).
    fn redraw(&mut self) -> bool {
        if self.gpu.is_none() || self.tabs.is_empty() {
            return false;
        }

        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let cell = gpu.cell;
        let colors = gpu.colors.clone();
        let theme = &self.theme;

        // Scene: panels + tab bar
        let mut scene = DrawContext::new();
        let mut scene_tab_text = Vec::new();
        let mut scene_panel_text = Vec::new();

        // Panel rendering
        let panel = &mut self.tabs[self.active_tab];
        panel.draw(
            &mut scene,
            &mut scene_panel_text,
            &mut gpu.font_system,
            &colors,
            &cell,
            &theme.panel,
        );
        let panel_bufs = panel.buffers();

        // Tab bar rendering
        let area = Rect {
            x: theme.general.panel_area_padding * scale,
            y: theme.general.panel_area_padding * scale,
            width: gpu.surface_config.width as f32 - 2.0 * theme.general.panel_area_padding * scale,
            height: gpu.surface_config.height as f32 - 2.0 * theme.general.panel_area_padding * scale,
        };
        self.tab_bar.draw(
            &mut scene,
            &mut scene_tab_text,
            theme,
            scale,
            area.x,
            area.y,
            area.width,
        );
        let tab_bufs = self.tab_bar.tab_buffers();

        // Overlay: dropdown
        let mut overlay = DrawContext::new();
        let mut overlay_dd_text = Vec::new();
        if self.dropdown.is_open() {
            self.dropdown.draw(&mut overlay, &mut overlay_dd_text, theme, scale);
        }
        let dd_bufs = self.dropdown.item_buffers();

        let scene_text: Vec<(&[crate::layout::TextSpec], &[glyphon::Buffer])> = vec![
            (&scene_tab_text, tab_bufs),
            (&scene_panel_text, panel_bufs),
        ];
        let overlay_text: Vec<(&[crate::layout::TextSpec], &[glyphon::Buffer])> = vec![
            (&overlay_dd_text, dd_bufs),
        ];

        let screenshot = self.screenshot_pending.take();
        let took_screenshot = screenshot.is_some();
        match gpu.render_frame(
            &scene,
            &overlay,
            &scene_text,
            &overlay_text,
            &self.icon_manager,
            screenshot.as_deref(),
        ) {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let w = gpu.surface_config.width;
                let h = gpu.surface_config.height;
                gpu.resize(w, h);
            }
            Err(wgpu::SurfaceError::Timeout) => {
                log::warn!("surface timeout");
            }
            Err(e) => {
                log::error!("render error: {e}");
            }
        }
        took_screenshot
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn clamp_active_tab(&mut self) {
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
    }

    fn sync_tab_state(&mut self) {
        self.update_viewports();
        self.update_tab_bar();
        self.update_window_title();
        self.request_redraw();
    }

    fn update_window_title(&self) {
        if let Some(w) = &self.window
            && let Some(panel) = self.tabs.get(self.active_tab)
        {
            w.set_title(panel.title());
        }
    }

    fn add_tab(&mut self, panel: TerminalPanel) {
        self.tabs.push(panel);
        self.active_tab = self.tabs.len() - 1;
        self.sync_tab_state();
    }

    fn new_tab(&mut self, shell: Option<String>) {
        let gpu = self.gpu.as_ref().unwrap();
        let panel = self.create_terminal_panel(gpu, shell, Vec::new());
        self.add_tab(panel);
    }

    fn new_tab_ssh(&mut self, result: SshResult) {
        self.connect_ssh(result.to_ssh_config());
    }

    fn connect_ssh(&mut self, config: crate::ssh::SshConfig) {
        let gpu = self.gpu.as_ref().unwrap();
        let (panel_id, vp, event_proxy) = self.new_panel_params(gpu);
        let size = TermSize::new(vp.cols, vp.rows);
        let panel = TerminalPanel::new_ssh(panel_id, size, event_proxy, config);
        self.add_tab(panel);
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);

        if self.tabs.is_empty() {
            // Last tab closed — open a fresh one instead of exiting
            self.new_tab(None);
            return;
        }

        self.clamp_active_tab();
        self.sync_tab_state();
    }

    fn open_new_tab_dropdown(&mut self) {
        let shells = detect_shells();
        let saved = &self.saved_sessions.sessions;

        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let surface_w = gpu.surface_config.width as f32;
        let surface_h = gpu.surface_config.height as f32;

        let mut entries: Vec<MenuEntry> = shells
            .into_iter()
            .map(|(label, path)| MenuEntry::item(&label, MenuAction::NewShell(path)))
            .collect();

        // Separator between shells and SSH section
        entries.push(MenuEntry::Separator);

        // Saved SSH sessions (sorted by last used)
        for session in saved {
            entries.push(MenuEntry::closeable_item_with_icon(
                &session.display_label(),
                MenuAction::ConnectSavedSession(session.key()),
                icons::ICON_TERMINAL,
            ));
        }

        // Separator before "New SSH Session..." (only if saved sessions exist)
        if !saved.is_empty() {
            entries.push(MenuEntry::Separator);
        }

        entries.push(MenuEntry::item_with_icon(
            "New SSH Session...",
            MenuAction::OpenSshDialog,
            icons::ICON_ADD,
        ));

        let anchor = self.tab_bar.plus_rect();
        self.dropdown.open(
            entries,
            anchor,
            Some(280.0),
            scale,
            surface_w,
            surface_h,
            &mut gpu.font_system,
            &self.theme.dropdown,
        );
    }

    fn open_ssh_dialog(&mut self, event_loop: &ActiveEventLoop, prefill: Option<SshPrefill>) {
        if self.ssh_dialog_window.is_some() {
            return; // already open
        }
        self.ssh_dialog_window =
            Some(SshDialogWindow::open(event_loop, &self.theme, prefill.as_ref()));
    }

    fn save_ssh_session(&mut self, result: &SshResult) {
        let saved = SavedSession {
            host: result.host.clone(),
            port: result.port.parse().unwrap_or(22),
            username: result.username.clone(),
            auth_type: match result.auth_method {
                AuthMethod::Password => SavedAuthType::Password,
                AuthMethod::Key => SavedAuthType::Key,
                AuthMethod::Agent => SavedAuthType::Agent,
            },
            key_path: match result.auth_method {
                AuthMethod::Key => Some(result.key_path.clone()),
                _ => None,
            },
            last_used: now_unix(),
        };
        self.saved_sessions.upsert(saved);
    }

    fn execute_menu_action(&mut self, action: &MenuAction, event_loop: &ActiveEventLoop) {
        match action {
            MenuAction::NewShell(shell_path) => {
                self.new_tab(Some(shell_path.clone()));
            }
            MenuAction::OpenSshDialog => {
                self.open_ssh_dialog(event_loop, None);
            }
            MenuAction::ConnectSavedSession(key) => {
                if let Some(session) = self.saved_sessions.find_by_key(key) {
                    let config = crate::ssh::SshConfig {
                        host: session.host.clone(),
                        port: session.port,
                        username: session.username.clone(),
                        auth: match &session.auth_type {
                            SavedAuthType::Password => {
                                crate::ssh::SshAuth::Password(String::new())
                            }
                            SavedAuthType::Key => crate::ssh::SshAuth::Key {
                                path: session
                                    .key_path
                                    .clone()
                                    .unwrap_or_else(|| "~/.ssh/id_ed25519".to_string()),
                                passphrase: None,
                            },
                            SavedAuthType::Agent => crate::ssh::SshAuth::Agent,
                        },
                    };
                    self.saved_sessions.touch_by_key(key);
                    self.connect_ssh(config);
                }
            }
        }
    }
}

impl ApplicationHandler<TerminalEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("pfauterminal")
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let gpu = GpuContext::new(window.clone(), self.theme.colors.clone());

        self.window = Some(window);
        self.gpu = Some(gpu);

        self.new_tab(None);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TerminalEvent) {
        match event {
            TerminalEvent::Wakeup => {
                self.request_redraw();
            }
            TerminalEvent::Title(panel_id, title) => {
                if let Some(panel) = self.tabs.iter_mut().find(|p| p.id() == panel_id) {
                    panel.set_title(title);
                }
                self.sync_tab_state();
            }
            TerminalEvent::SshDialogClose(result) => {
                self.ssh_dialog_window = None;
                if let Some(result) = result
                    && !result.host.is_empty()
                {
                    self.save_ssh_session(&result);
                    self.new_tab_ssh(result);
                }
            }
            TerminalEvent::Exit(panel_id) => {
                log::info!("terminal {panel_id:?} exited");
                self.tabs.retain(|p| p.id() != panel_id);

                if self.tabs.is_empty() {
                    event_loop.exit();
                    return;
                }
                self.clamp_active_tab();
                self.sync_tab_state();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Route events to SSH dialog window if it matches
        if let Some(dialog_win) = &mut self.ssh_dialog_window
            && window_id == dialog_win.window_id()
        {
            match dialog_win.handle_event(event) {
                Ok(None) => {} // continue
                Ok(Some(result)) => {
                    // Defer close — dropping the window inside its own event
                    // handler (e.g. key_down) crashes on macOS because the
                    // NSView is deallocated while Objective-C still holds it.
                    let _ = self
                        .event_proxy_raw
                        .send_event(TerminalEvent::SshDialogClose(Some(result)));
                }
                Err(()) => {
                    // Defer cancel
                    let _ = self
                        .event_proxy_raw
                        .send_event(TerminalEvent::SshDialogClose(None));
                }
            }
            return;
        }

        // Ignore events for unknown windows (stale events from recently closed dialog)
        if let Some(main_window) = &self.window
            && window_id != main_window.id()
        {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(new_size) => {
                self.dropdown.close();
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(new_size.width, new_size.height);
                }
                self.sync_tab_state();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dropdown.close();
                if let Some(gpu) = &mut self.gpu {
                    gpu.scale_factor = scale_factor as f32;
                }
                self.sync_tab_state();
            }

            WindowEvent::RedrawRequested => {
                self.redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                // position is already in physical pixels
                self.cursor_position = (position.x as f32, position.y as f32);
                let (cx, cy) = self.cursor_position;

                // Dropdown hover takes priority when open
                if self.dropdown.is_open() {
                    let hover = self.dropdown.hit_test(cx, cy);
                    if self.dropdown.set_hover(hover) {
                        self.request_redraw();
                    }
                    return;
                }

                // Drag selection
                if self.mouse_left_pressed {
                    if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                        if let Some((point, side)) = panel.pixel_to_point(cx, cy) {
                            panel.update_selection(point, side);
                            self.request_redraw();
                        }
                    }
                }

                // Update tab bar hover state
                let scale = self.gpu.as_ref().map(|g| g.scale_factor).unwrap_or(1.0);
                let pad = self.theme.general.panel_area_padding * scale;
                let tab_h = TabBar::height(&self.theme.tab_bar, scale);
                let hover = if cy >= pad && cy < pad + tab_h {
                    self.tab_bar.hit_test(cx, cy)
                } else {
                    TabBarElement::None
                };
                if self.tab_bar.set_hover(hover) {
                    self.request_redraw();
                }

                // Set cursor icon: text (I-beam) over terminal content, default elsewhere
                if let Some(window) = &self.window {
                    let in_content = self
                        .tabs
                        .get(self.active_tab)
                        .is_some_and(|p| p.is_in_content_area(cx, cy));
                    window.set_cursor(if in_content {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Default
                    });
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if self.tab_bar.set_hover(TabBarElement::None) {
                    self.request_redraw();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (cx, cy) = self.cursor_position;

                // Dropdown intercepts all clicks when open
                if self.dropdown.is_open() {
                    match self.dropdown.hit_test(cx, cy) {
                        DropdownElement::Item(idx) => {
                            let action = self.dropdown.action_for(idx).cloned();
                            self.dropdown.close();
                            if let Some(action) = action {
                                self.execute_menu_action(&action, event_loop);
                            }
                        }
                        DropdownElement::CloseButton(idx) => {
                            if let Some(MenuAction::ConnectSavedSession(key)) =
                                self.dropdown.action_for(idx).cloned()
                            {
                                self.saved_sessions.remove_by_key(&key);
                            }
                            self.dropdown.close();
                        }
                        DropdownElement::None => {
                            if self.dropdown.is_outside(cx, cy) {
                                self.dropdown.close();
                            }
                        }
                    }
                    self.request_redraw();
                    return;
                }

                let scale = self.gpu.as_ref().map(|g| g.scale_factor).unwrap_or(1.0);
                let pad = self.theme.general.panel_area_padding * scale;
                let tab_h = TabBar::height(&self.theme.tab_bar, scale);

                if cy >= pad && cy < pad + tab_h {
                    match self.tab_bar.hit_test(cx, cy) {
                        TabBarElement::Tab(idx) => {
                            if idx < self.tabs.len() {
                                self.active_tab = idx;
                                self.sync_tab_state();
                            }
                        }
                        TabBarElement::CloseButton(idx) => {
                            if idx < self.tabs.len() {
                                self.close_tab(idx);
                            }
                        }
                        TabBarElement::PlusButton => {
                            self.open_new_tab_dropdown();
                            self.request_redraw();
                        }
                        TabBarElement::None => {}
                    }
                } else if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                    if let Some((point, side)) = panel.pixel_to_point(cx, cy) {
                        let now = Instant::now();
                        if now.duration_since(self.last_click_time).as_millis() < 400 {
                            self.click_count = (self.click_count + 1).min(3);
                        } else {
                            self.click_count = 1;
                        }
                        self.last_click_time = now;

                        let ty = match self.click_count {
                            2 => SelectionType::Semantic,
                            3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };

                        panel.start_selection(ty, point, side);
                        self.mouse_left_pressed = true;
                        self.request_redraw();
                    }
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.mouse_left_pressed = false;
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.super_pressed = new_modifiers.state().super_key();
                self.ctrl_pressed = new_modifiers.state().control_key();
                self.alt_pressed = new_modifiers.state().alt_key();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // Close dropdown on Escape
                if self.dropdown.is_open()
                    && event.state == ElementState::Pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                {
                    self.dropdown.close();
                    self.request_redraw();
                    return;
                }

                if event.state == ElementState::Pressed && self.super_pressed {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyC) => {
                            if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                                if let Some(text) = panel.selection_to_string() {
                                    if let Ok(mut clip) = arboard::Clipboard::new() {
                                        let _ = clip.set_text(text);
                                    }
                                    panel.clear_selection();
                                    self.request_redraw();
                                }
                            }
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyV) => {
                            if let Ok(mut clip) = arboard::Clipboard::new()
                                && let Ok(text) = clip.get_text()
                                && let Some(panel) = self.tabs.get(self.active_tab)
                            {
                                panel.write_to_pty(text.into_bytes());
                            }
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyS) => {
                            self.screenshot_pending =
                                Some("/tmp/pfauterminal_screenshot.png".to_string());
                            self.request_redraw();
                            log::info!("screenshot requested");
                            return;
                        }
                        _ => {}
                    }
                    // Don't pass Cmd+key combos to the terminal
                    return;
                }

                if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                    panel.handle_key(&event, self.ctrl_pressed, self.alt_pressed);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let cell_height = self
                    .gpu
                    .as_ref()
                    .map(|g| g.cell.height as f64 * g.scale_factor as f64)
                    .unwrap_or(16.0);
                if let Some(panel) = self.tabs.get_mut(self.active_tab)
                    && panel.handle_scroll(delta, cell_height)
                {
                    self.request_redraw();
                }
            }

            _ => {}
        }
    }
}

/// Detect available shells on the system. Returns (display_label, full_path) pairs.
fn detect_shells() -> Vec<(String, String)> {
    let mut shells = Vec::new();
    let mut seen = std::collections::HashSet::new();

    #[cfg(not(windows))]
    {
        // (shell_name, candidate_paths) — checked in order, first existing path wins
        const SHELLS: &[(&str, &[&str])] = &[
            ("zsh", &["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"]),
            ("bash", &["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]),
            ("fish", &["/usr/bin/fish", "/usr/local/bin/fish"]),
            ("nu", &["/usr/bin/nu", "/usr/local/bin/nu"]),
            ("pwsh", &["/usr/bin/pwsh", "/usr/local/bin/pwsh"]),
            ("sh", &["/bin/sh"]),
        ];

        // Prefer paths from /etc/shells if available
        if let Ok(contents) = std::fs::read_to_string("/etc/shells") {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let path = std::path::Path::new(line);
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if SHELLS.iter().any(|(n, _)| *n == name)
                    && path.exists()
                    && seen.insert(name.to_string())
                {
                    shells.push((name.to_string(), line.to_string()));
                }
            }
        }

        // Fallback: check candidate paths directly
        if shells.is_empty() {
            for (name, candidates) in SHELLS {
                if seen.contains(*name) {
                    continue;
                }
                for path in *candidates {
                    if std::path::Path::new(path).exists() {
                        seen.insert(name.to_string());
                        shells.push((name.to_string(), path.to_string()));
                        break;
                    }
                }
            }
        }

        if shells.is_empty() {
            shells.push(("sh".to_string(), "/bin/sh".to_string()));
        }
    }

    #[cfg(windows)]
    {
        // PowerShell 7+ (pwsh) — found via PATH
        if let Ok(output) = std::process::Command::new("where").arg("pwsh").output() {
            if output.status.success() {
                if let Some(path) = String::from_utf8_lossy(&output.stdout).lines().next() {
                    let path = path.trim();
                    if seen.insert("pwsh".to_string()) {
                        shells.push(("PowerShell".to_string(), path.to_string()));
                    }
                }
            }
        }

        // SystemRoot-based shells
        if let Some(sysroot) = std::env::var_os("SystemRoot") {
            let sysroot = std::path::Path::new(&sysroot);
            let candidates: &[(&str, &str, std::path::PathBuf)] = &[
                (
                    "powershell",
                    "Windows PowerShell",
                    sysroot
                        .join("System32")
                        .join("WindowsPowerShell")
                        .join("v1.0")
                        .join("powershell.exe"),
                ),
                (
                    "cmd",
                    "Command Prompt",
                    sysroot.join("System32").join("cmd.exe"),
                ),
            ];
            for (key, label, path) in candidates {
                if path.exists() {
                    if let Some(p) = path.to_str() {
                        if seen.insert(key.to_string()) {
                            shells.push((label.to_string(), p.to_string()));
                        }
                    }
                }
            }
        }

        // Git Bash
        for path in &[
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "C:\\Program Files (x86)\\Git\\bin\\bash.exe",
        ] {
            if std::path::Path::new(path).exists() {
                if seen.insert("git-bash".to_string()) {
                    shells.push(("Git Bash".to_string(), path.to_string()));
                    break;
                }
            }
        }

        if shells.is_empty() {
            shells.push(("cmd".to_string(), "cmd.exe".to_string()));
        }
    }

    shells
}
