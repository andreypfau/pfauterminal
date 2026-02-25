use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::colors::ColorScheme;
use crate::dropdown::{DropdownHit, DropdownMenu, MenuAction, MenuItem};
use crate::gpu::GpuContext;
use crate::icons::IconManager;
use crate::layout::Rect;
use crate::ssh_dialog::{SshDialogWindow, SshResult};
use crate::tab_bar::{TabBar, TabBarHit, TabBarHover};
use crate::terminal::{EventProxy, TerminalEvent};
use crate::terminal_panel::{PanelId, TerminalPanel};

/// Padding between window edges and panel area (logical pixels).
const PANEL_AREA_PADDING: f32 = 8.0;

pub struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    icon_manager: IconManager,
    event_proxy_raw: EventLoopProxy<TerminalEvent>,
    colors: ColorScheme,
    workspaces: Vec<TerminalPanel>,
    active_workspace: usize,
    tab_bar: TabBar,
    dropdown: DropdownMenu,
    ssh_dialog_window: Option<SshDialogWindow>,
    cursor_position: (f32, f32),
    super_pressed: bool,
    screenshot_pending: Option<String>,
}

impl App {
    pub fn new(event_proxy_raw: EventLoopProxy<TerminalEvent>) -> Self {
        let colors = ColorScheme::load();
        Self {
            window: None,
            gpu: None,
            icon_manager: IconManager::new(),
            event_proxy_raw,
            colors,
            workspaces: Vec::new(),
            active_workspace: 0,
            tab_bar: TabBar::new(),
            dropdown: DropdownMenu::new(),
            ssh_dialog_window: None,
            cursor_position: (0.0, 0.0),
            super_pressed: false,
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
        let area = Self::panel_area_from_gpu(gpu);
        let tab_h = TabBar::height(scale);
        let vp = TerminalPanel::compute_viewport(&area, &gpu.cell, scale, tab_h);
        (panel_id, vp, event_proxy)
    }

    fn create_terminal_panel(
        &self,
        gpu: &GpuContext,
        shell: Option<String>,
        args: Vec<String>,
    ) -> TerminalPanel {
        let (panel_id, vp, event_proxy) = self.new_panel_params(gpu);
        let cell_w = (gpu.cell.width * gpu.scale_factor) as u16;
        let cell_h = (gpu.cell.height * gpu.scale_factor) as u16;

        TerminalPanel::new(
            panel_id,
            vp.cols,
            vp.rows,
            cell_w,
            cell_h,
            event_proxy,
            shell,
            args,
        )
    }

    fn panel_area_from_gpu(gpu: &GpuContext) -> Rect {
        let scale = gpu.scale_factor;
        let pad = PANEL_AREA_PADDING * scale;
        Rect {
            x: pad,
            y: pad,
            width: gpu.surface_config.width as f32 - 2.0 * pad,
            height: gpu.surface_config.height as f32 - 2.0 * pad,
        }
    }

    fn update_viewports(&mut self) {
        let gpu = self.gpu.as_ref().unwrap();
        let cell = gpu.cell.clone();
        let scale = gpu.scale_factor;
        let area = Self::panel_area_from_gpu(gpu);
        let tab_h = TabBar::height(scale);

        if let Some(panel) = self.workspaces.get_mut(self.active_workspace) {
            let viewport = TerminalPanel::compute_viewport(&area, &cell, scale, tab_h);
            panel.set_viewport(viewport, &cell);
        }
    }

    fn update_tab_bar(&mut self) {
        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let pad = PANEL_AREA_PADDING * scale;
        let panel_width = gpu.surface_config.width as f32 - 2.0 * pad;

        let titles: Vec<String> = self
            .workspaces
            .iter()
            .map(|p| p.title().to_string())
            .collect();

        self.tab_bar.update(
            &titles,
            self.active_workspace,
            panel_width,
            pad,
            scale,
            &mut gpu.font_system,
        );
    }

    /// Returns true if a screenshot was captured (signals auto-exit).
    fn redraw(&mut self) -> bool {
        if self.gpu.is_none() || self.workspaces.is_empty() {
            return false;
        }

        let gpu = self.gpu.as_mut().unwrap();

        // Prepare render data from active panel
        let panel = &mut self.workspaces[self.active_workspace];

        let draw = panel.prepare_render(&mut gpu.font_system, &gpu.colors);
        let panel_draws = vec![draw];
        let bufs = panel.buffers();
        let panel_buf_refs = vec![bufs];

        let dropdown_ref = if self.dropdown.is_open() {
            Some(&self.dropdown)
        } else {
            None
        };

        let screenshot = self.screenshot_pending.take();
        let took_screenshot = screenshot.is_some();
        match gpu.render_frame(
            &self.tab_bar,
            dropdown_ref,
            &panel_draws,
            &panel_buf_refs,
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

    fn sync_workspace_state(&mut self) {
        self.update_viewports();
        self.update_tab_bar();
        self.update_window_title();
        self.request_redraw();
    }

    fn update_window_title(&self) {
        if let Some(w) = &self.window {
            if let Some(panel) = self.workspaces.get(self.active_workspace) {
                w.set_title(panel.title());
            }
        }
    }

    fn add_workspace(&mut self, panel: TerminalPanel) {
        self.workspaces.push(panel);
        self.active_workspace = self.workspaces.len() - 1;
        self.sync_workspace_state();
    }

    fn new_workspace(&mut self, shell: Option<String>) {
        let gpu = self.gpu.as_ref().unwrap();
        let panel = self.create_terminal_panel(gpu, shell, Vec::new());
        self.add_workspace(panel);
    }

    fn new_workspace_ssh(&mut self, result: SshResult) {
        let gpu = self.gpu.as_ref().unwrap();
        let (panel_id, vp, event_proxy) = self.new_panel_params(gpu);
        let panel = TerminalPanel::new_ssh(
            panel_id,
            vp.cols,
            vp.rows,
            event_proxy,
            result.to_ssh_config(),
        );
        self.add_workspace(panel);
    }

    fn close_workspace(&mut self, idx: usize) {
        if idx >= self.workspaces.len() {
            return;
        }
        self.workspaces.remove(idx);

        if self.workspaces.is_empty() {
            // Last tab closed — open a fresh one instead of exiting
            self.new_workspace(None);
            return;
        }

        if self.active_workspace >= self.workspaces.len() {
            self.active_workspace = self.workspaces.len() - 1;
        }
        self.sync_workspace_state();
    }

    fn open_new_tab_dropdown(&mut self) {
        let shells = detect_shells();

        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let surface_w = gpu.surface_config.width as f32;
        let surface_h = gpu.surface_config.height as f32;

        let mut items: Vec<MenuItem> = shells
            .into_iter()
            .map(|(label, path)| MenuItem {
                label,
                action: MenuAction::NewShell(path),
            })
            .collect();

        items.push(MenuItem {
            label: "SSH Session...".to_string(),
            action: MenuAction::OpenSshDialog,
        });

        let anchor = self.tab_bar.plus_rect();
        self.dropdown.open(
            items,
            anchor,
            scale,
            surface_w,
            surface_h,
            &mut gpu.font_system,
        );
    }

    fn open_ssh_dialog(&mut self, event_loop: &ActiveEventLoop) {
        if self.ssh_dialog_window.is_some() {
            return; // already open
        }
        self.ssh_dialog_window = Some(SshDialogWindow::open(event_loop));
    }

    fn execute_menu_action(&mut self, action: &MenuAction, event_loop: &ActiveEventLoop) {
        match action {
            MenuAction::NewShell(shell_path) => {
                self.new_workspace(Some(shell_path.clone()));
            }
            MenuAction::OpenSshDialog => {
                self.open_ssh_dialog(event_loop);
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
        let gpu = GpuContext::new(window.clone(), self.colors.clone());

        self.window = Some(window);
        self.gpu = Some(gpu);

        self.new_workspace(None);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TerminalEvent) {
        match event {
            TerminalEvent::Wakeup => {
                self.request_redraw();
            }
            TerminalEvent::Title(panel_id, title) => {
                if let Some(panel) = self.workspaces.iter_mut().find(|p| p.id() == panel_id) {
                    panel.set_title(title);
                }
                self.sync_workspace_state();
            }
            TerminalEvent::SshDialogClose(result) => {
                self.ssh_dialog_window = None;
                if let Some(result) = result {
                    if !result.host.is_empty() {
                        self.new_workspace_ssh(result);
                    }
                }
            }
            TerminalEvent::Exit(panel_id) => {
                log::info!("terminal {panel_id:?} exited");
                self.workspaces.retain(|p| p.id() != panel_id);

                if self.workspaces.is_empty() {
                    event_loop.exit();
                    return;
                }
                if self.active_workspace >= self.workspaces.len() {
                    self.active_workspace = self.workspaces.len() - 1;
                }
                self.sync_workspace_state();
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
        if let Some(dialog_win) = &mut self.ssh_dialog_window {
            if window_id == dialog_win.window_id() {
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
        }

        // Ignore events for unknown windows (stale events from recently closed dialog)
        if let Some(main_window) = &self.window {
            if window_id != main_window.id() {
                return;
            }
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
                self.sync_workspace_state();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dropdown.close();
                if let Some(gpu) = &mut self.gpu {
                    gpu.scale_factor = scale_factor as f32;
                }
                self.sync_workspace_state();
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
                    let hover = self.dropdown.compute_hover(cx, cy);
                    if self.dropdown.set_hover(hover) {
                        self.request_redraw();
                    }
                    return;
                }

                // Update tab bar hover state
                let scale = self.gpu.as_ref().map(|g| g.scale_factor).unwrap_or(1.0);
                let pad = PANEL_AREA_PADDING * scale;
                let tab_h = TabBar::height(scale);
                let hover = if cy >= pad && cy < pad + tab_h {
                    self.tab_bar.compute_hover(cx, cy)
                } else {
                    TabBarHover::None
                };
                if self.tab_bar.set_hover(hover) {
                    self.request_redraw();
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if self.tab_bar.set_hover(TabBarHover::None) {
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
                        DropdownHit::Item(idx) => {
                            let action = self.dropdown.action_for(idx).cloned();
                            self.dropdown.close();
                            if let Some(action) = action {
                                self.execute_menu_action(&action, event_loop);
                            }
                        }
                        DropdownHit::Outside => {
                            self.dropdown.close();
                        }
                        DropdownHit::None => {}
                    }
                    self.request_redraw();
                    return;
                }

                let scale = self.gpu.as_ref().map(|g| g.scale_factor).unwrap_or(1.0);
                let pad = PANEL_AREA_PADDING * scale;
                let tab_h = TabBar::height(scale);

                if cy >= pad && cy < pad + tab_h {
                    match self.tab_bar.hit_test(cx, cy) {
                        TabBarHit::Tab(idx) => {
                            if idx < self.workspaces.len() {
                                self.active_workspace = idx;
                                self.tab_bar.set_active(idx);
                                self.sync_workspace_state();
                            }
                        }
                        TabBarHit::CloseTab(idx) => {
                            if idx < self.workspaces.len() {
                                self.close_workspace(idx);
                            }
                        }
                        TabBarHit::NewTab => {
                            self.open_new_tab_dropdown();
                            self.request_redraw();
                        }
                        TabBarHit::None => {}
                    }
                }
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.super_pressed = new_modifiers.state().super_key();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // Close dropdown on Escape
                if self.dropdown.is_open() {
                    if event.state == ElementState::Pressed {
                        if let PhysicalKey::Code(KeyCode::Escape) = event.physical_key {
                            self.dropdown.close();
                            self.request_redraw();
                            return;
                        }
                    }
                }

                if event.state == ElementState::Pressed && self.super_pressed {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyV) => {
                            if let Ok(mut clip) = arboard::Clipboard::new() {
                                if let Ok(text) = clip.get_text() {
                                    if let Some(panel) = self.workspaces.get(self.active_workspace)
                                    {
                                        panel.write_to_pty(text.into_bytes());
                                    }
                                }
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
                }

                if let Some(panel) = self.workspaces.get_mut(self.active_workspace) {
                    panel.handle_key(&event);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let cell_height = self
                    .gpu
                    .as_ref()
                    .map(|g| g.cell.height as f64)
                    .unwrap_or(16.0);
                if let Some(panel) = self.workspaces.get_mut(self.active_workspace) {
                    if panel.handle_scroll(delta, cell_height) {
                        self.request_redraw();
                    }
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
        const COMMON_SHELLS: &[&str] = &["zsh", "bash", "fish", "nu", "pwsh"];

        if let Ok(contents) = std::fs::read_to_string("/etc/shells") {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let path = std::path::Path::new(line);
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !COMMON_SHELLS.contains(&name) {
                    continue;
                }
                if path.exists() && seen.insert(name.to_string()) {
                    shells.push((name.to_string(), line.to_string()));
                }
            }
        }

        // Fallback: check well-known paths
        if shells.is_empty() {
            for path in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
                if std::path::Path::new(path).exists() {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(path);
                    if seen.insert(name.to_string()) {
                        shells.push((name.to_string(), path.to_string()));
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
        // PowerShell 7+ (pwsh)
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

        // Windows PowerShell (powershell.exe)
        if let Some(sysroot) = std::env::var_os("SystemRoot") {
            let ps_path = std::path::Path::new(&sysroot)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe");
            if ps_path.exists() {
                if let Some(p) = ps_path.to_str() {
                    if seen.insert("powershell".to_string()) {
                        shells.push(("Windows PowerShell".to_string(), p.to_string()));
                    }
                }
            }
        }

        // cmd.exe
        if let Some(sysroot) = std::env::var_os("SystemRoot") {
            let cmd_path = std::path::Path::new(&sysroot)
                .join("System32")
                .join("cmd.exe");
            if cmd_path.exists() {
                if let Some(p) = cmd_path.to_str() {
                    if seen.insert("cmd".to_string()) {
                        shells.push(("Command Prompt".to_string(), p.to_string()));
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
