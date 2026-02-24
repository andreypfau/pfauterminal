use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::colors::ColorScheme;
use crate::dropdown::{DropdownHit, DropdownMenu, MenuAction, MenuItem};
use crate::gpu::GpuContext;
use crate::icons::IconManager;
use crate::layout::Rect;
use crate::panel::{PanelAction, PanelId};
use crate::panels::terminal::TerminalPanel;
use crate::tab_bar::{TabBar, TabBarHit};
use crate::terminal::{EventProxy, TerminalEvent};
use crate::workspace::Workspace;

/// Padding between window edges and panel area (logical pixels).
const PANEL_AREA_PADDING: f32 = 8.0;

pub struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    icon_manager: IconManager,
    event_proxy_raw: EventLoopProxy<TerminalEvent>,
    colors: ColorScheme,
    workspaces: Vec<Workspace>,
    active_workspace: usize,
    tab_bar: TabBar,
    dropdown: DropdownMenu,
    cursor_position: (f32, f32),
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
            cursor_position: (0.0, 0.0),
            screenshot_pending: std::env::var("SCREENSHOT").ok().filter(|s| !s.is_empty()),
        }
    }

    fn create_terminal_panel(&self, gpu: &GpuContext, shell: Option<String>) -> TerminalPanel {
        let panel_id = PanelId::next();
        let event_proxy = EventProxy::new(self.event_proxy_raw.clone(), panel_id);

        // Compute initial size from full panel area (including tab bar space)
        let scale = gpu.scale_factor;
        let pad = PANEL_AREA_PADDING * scale;
        let tab_h = TabBar::height(scale);
        let panel_rect = Rect {
            x: pad,
            y: pad,
            width: gpu.surface_width() as f32 - 2.0 * pad,
            height: gpu.surface_height() as f32 - 2.0 * pad,
        };

        let (_, inner) = TerminalPanel::compute_island_rects_static(&panel_rect, scale, tab_h);
        let (cols, rows) = TerminalPanel::compute_grid_size_static(&inner, &gpu.cell, scale);

        let cell_w = (gpu.cell.width * scale) as u16;
        let cell_h = (gpu.cell.height * scale) as u16;

        TerminalPanel::new(panel_id, cols, rows, cell_w, cell_h, event_proxy, shell)
    }

    /// Full panel rect including tab bar space — the panel island covers this.
    fn panel_area(&self) -> Rect {
        let gpu = self.gpu.as_ref().unwrap();
        let scale = gpu.scale_factor;
        let pad = PANEL_AREA_PADDING * scale;
        Rect {
            x: pad,
            y: pad,
            width: gpu.surface_width() as f32 - 2.0 * pad,
            height: gpu.surface_height() as f32 - 2.0 * pad,
        }
    }

    fn update_viewports(&mut self) {
        let gpu = self.gpu.as_ref().unwrap();
        let cell = gpu.cell.clone();
        let scale = gpu.scale_factor;
        let area = self.panel_area();
        let tab_h = TabBar::height(scale);

        if let Some(ws) = self.workspaces.get_mut(self.active_workspace) {
            ws.compute_viewports(area, &cell, scale, tab_h);
        }
    }

    fn update_tab_bar(&mut self) {
        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let pad = PANEL_AREA_PADDING * scale;
        let panel_width = gpu.surface_width() as f32 - 2.0 * pad;

        let titles: Vec<String> = self
            .workspaces
            .iter()
            .map(|ws| ws.title().to_string())
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
        let scale = gpu.scale_factor;

        // Prepare render data from active workspace
        let ws = &mut self.workspaces[self.active_workspace];

        let mut panel_draws = Vec::new();
        let mut panel_line_bufs_owned: Vec<Vec<&glyphon::Buffer>> = Vec::new();

        // Collect panel IDs first to avoid borrow issues
        let panel_ids: Vec<PanelId> = ws.panels.keys().copied().collect();

        for panel_id in &panel_ids {
            if let Some(panel) = ws.panels.get_mut(panel_id) {
                let draw = panel.prepare_render(&mut gpu.font_system, &gpu.colors);
                panel_draws.push(draw);
            }
        }

        // Now collect line buffer references
        for panel_id in &panel_ids {
            if let Some(panel) = ws.panels.get(panel_id) {
                let bufs: Vec<&glyphon::Buffer> = panel.line_buffers().iter().collect();
                panel_line_bufs_owned.push(bufs);
            }
        }

        let panel_line_refs: Vec<&[glyphon::Buffer]> = panel_ids
            .iter()
            .filter_map(|id| ws.panels.get(id).map(|p| p.line_buffers()))
            .collect();

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
            &panel_line_refs,
            scale,
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

    fn process_panel_actions(&mut self) {
        let mut panels_to_remove: Vec<(usize, PanelId)> = Vec::new();
        let mut title_changed = false;

        for (ws_idx, ws) in self.workspaces.iter_mut().enumerate() {
            let panel_ids: Vec<PanelId> = ws.panels.keys().copied().collect();
            for panel_id in panel_ids {
                if let Some(panel) = ws.panels.get_mut(&panel_id) {
                    let actions = panel.drain_actions();
                    for action in actions {
                        match action {
                            PanelAction::SetTitle(_title) => {
                                title_changed = true;
                            }
                            PanelAction::Close => {
                                panels_to_remove.push((ws_idx, panel_id));
                            }
                            PanelAction::Redraw => {}
                        }
                    }
                }
            }
        }

        // Remove panels
        for (ws_idx, panel_id) in panels_to_remove {
            if let Some(ws) = self.workspaces.get_mut(ws_idx) {
                ws.remove_panel(panel_id);
            }
        }

        // Remove empty workspaces
        self.workspaces.retain(|ws| !ws.is_empty());

        if self.workspaces.is_empty() {
            // All workspaces gone, exit
            std::process::exit(0);
        }

        // Fix active index
        if self.active_workspace >= self.workspaces.len() {
            self.active_workspace = self.workspaces.len() - 1;
        }

        if title_changed {
            self.update_window_title();
        }
    }

    fn update_window_title(&self) {
        if let Some(w) = &self.window {
            if let Some(ws) = self.workspaces.get(self.active_workspace) {
                w.set_title(ws.title());
            }
        }
    }

    fn new_workspace(&mut self, shell: Option<String>) {
        let gpu = self.gpu.as_ref().unwrap();
        let panel = self.create_terminal_panel(gpu, shell);
        let ws = Workspace::new(Box::new(panel));
        self.workspaces.push(ws);
        self.active_workspace = self.workspaces.len() - 1;
        self.update_viewports();
        self.update_tab_bar();
        self.update_window_title();
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
        self.update_viewports();
        self.update_tab_bar();
        self.update_window_title();
    }

    #[allow(dead_code)]
    fn find_panel_workspace(&self, panel_id: PanelId) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|ws| ws.panels.contains_key(&panel_id))
    }

    fn open_new_tab_dropdown(&mut self) {
        let shells = detect_shells();

        // If only one shell available, open it directly — no menu needed
        if shells.len() <= 1 {
            let shell = shells.into_iter().next().map(|(_, path)| path);
            self.new_workspace(shell);
            return;
        }

        let gpu = self.gpu.as_mut().unwrap();
        let scale = gpu.scale_factor;
        let surface_w = gpu.surface_width() as f32;
        let surface_h = gpu.surface_height() as f32;

        let items: Vec<MenuItem> = shells
            .into_iter()
            .map(|(label, path)| MenuItem {
                label,
                action: MenuAction::NewShell(path),
            })
            .collect();

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

    fn execute_menu_action(&mut self, action: &MenuAction) {
        match action {
            MenuAction::NewShell(shell_path) => {
                self.new_workspace(Some(shell_path.clone()));
            }
            MenuAction::Custom(_) => {}
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

        // Create first workspace with a terminal panel (system default shell)
        let panel = self.create_terminal_panel(&gpu, None);
        let ws = Workspace::new(Box::new(panel));
        self.workspaces.push(ws);

        self.window = Some(window);
        self.gpu = Some(gpu);

        self.update_viewports();
        self.update_tab_bar();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: TerminalEvent) {
        match event {
            TerminalEvent::Wakeup => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            TerminalEvent::Title(panel_id, title) => {
                // Find the panel and update its title
                for ws in &mut self.workspaces {
                    if let Some(panel) = ws.panels.get_mut(&panel_id) {
                        if let Some(tp) = panel.as_any_mut().downcast_mut::<TerminalPanel>() {
                            tp.set_title_from_event(title.clone());
                        }
                        break;
                    }
                }
                self.update_tab_bar();
                self.update_window_title();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            TerminalEvent::Exit(panel_id) => {
                log::info!("terminal {panel_id:?} exited");
                // Find and mark for removal
                for ws in &mut self.workspaces {
                    if let Some(panel) = ws.panels.get_mut(&panel_id) {
                        if let Some(tp) = panel.as_any_mut().downcast_mut::<TerminalPanel>() {
                            tp.mark_closed();
                        }
                        break;
                    }
                }
                self.process_panel_actions();
                self.update_tab_bar();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(new_size) => {
                self.dropdown.close();
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(new_size.width, new_size.height);
                }
                self.update_viewports();
                self.update_tab_bar();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dropdown.close();
                if let Some(gpu) = &mut self.gpu {
                    gpu.set_scale_factor(scale_factor as f32);
                }
                self.update_viewports();
                self.update_tab_bar();
            }

            WindowEvent::RedrawRequested => {
                if self.redraw() {
                    // Screenshot captured — auto-exit
                    event_loop.exit();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // position is already in physical pixels
                self.cursor_position = (position.x as f32, position.y as f32);
                let (cx, cy) = self.cursor_position;

                // Dropdown hover takes priority when open
                if self.dropdown.is_open() {
                    let hover = self.dropdown.compute_hover(cx, cy);
                    if self.dropdown.set_hover(hover) {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
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
                    crate::tab_bar::TabBarHover::None
                };
                if self.tab_bar.set_hover(hover) {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }

            WindowEvent::CursorLeft { .. } => {
                if self.tab_bar.set_hover(crate::tab_bar::TabBarHover::None) {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
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
                                self.execute_menu_action(&action);
                            }
                        }
                        DropdownHit::Outside => {
                            self.dropdown.close();
                        }
                        DropdownHit::None => {
                            // Clicked on menu padding — do nothing
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }

                let scale = self.gpu.as_ref().map(|g| g.scale_factor).unwrap_or(1.0);
                let pad = PANEL_AREA_PADDING * scale;
                let tab_h = TabBar::height(scale);

                if cy >= pad && cy < pad + tab_h {
                    // Hit test tab bar
                    match self.tab_bar.hit_test(cx, cy) {
                        TabBarHit::Tab(idx) => {
                            if idx < self.workspaces.len() {
                                self.active_workspace = idx;
                                self.tab_bar.set_active(idx);
                                self.update_viewports();
                                self.update_tab_bar();
                                self.update_window_title();
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        TabBarHit::CloseTab(idx) => {
                            if idx < self.workspaces.len() {
                                self.close_workspace(idx);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        TabBarHit::NewTab => {
                            self.open_new_tab_dropdown();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                        TabBarHit::None => {}
                    }
                } else {
                    // Hit test panels
                    let area = self.panel_area();
                    if let Some(ws) = self.workspaces.get_mut(self.active_workspace) {
                        if let Some(panel_id) = ws.hit_test(area, cx, cy) {
                            ws.focused_panel = panel_id;
                        }
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // Close dropdown on Escape
                if self.dropdown.is_open() {
                    if event.state == ElementState::Pressed {
                        if let winit::keyboard::PhysicalKey::Code(
                            winit::keyboard::KeyCode::Escape,
                        ) = event.physical_key
                        {
                            self.dropdown.close();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                            return;
                        }
                    }
                }

                if let Some(ws) = self.workspaces.get_mut(self.active_workspace) {
                    if let Some(panel) = ws.panels.get_mut(&ws.focused_panel) {
                        panel.handle_key(&event);
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(ws) = self.workspaces.get_mut(self.active_workspace) {
                    let cell_height = self
                        .gpu
                        .as_ref()
                        .map(|g| g.cell.height as f64)
                        .unwrap_or(16.0);
                    if let Some(panel) = ws.panels.get_mut(&ws.focused_panel) {
                        if panel.handle_scroll(delta, cell_height) {
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

/// Detect available shells on the system. Returns (display_label, full_path) pairs.
#[cfg(not(windows))]
fn detect_shells() -> Vec<(String, String)> {
    let mut shells = Vec::new();
    let mut seen = std::collections::HashSet::new();
    detect_shells_unix(&mut shells, &mut seen);
    if shells.is_empty() {
        shells.push(("sh".to_string(), "/bin/sh".to_string()));
    }
    shells
}

/// Detect available shells on the system. Returns (display_label, full_path) pairs.
#[cfg(windows)]
fn detect_shells() -> Vec<(String, String)> {
    let mut shells = Vec::new();
    let mut seen = std::collections::HashSet::new();
    detect_shells_windows(&mut shells, &mut seen);
    if shells.is_empty() {
        shells.push(("cmd".to_string(), "cmd.exe".to_string()));
    }
    shells
}

#[cfg(not(windows))]
fn detect_shells_unix(
    shells: &mut Vec<(String, String)>,
    seen: &mut std::collections::HashSet<String>,
) {
    // Only show commonly-used shells from /etc/shells
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
}

#[cfg(windows)]
fn detect_shells_windows(
    shells: &mut Vec<(String, String)>,
    seen: &mut std::collections::HashSet<String>,
) {
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

    // Windows PowerShell (powershell.exe) — always available on modern Windows
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
    let git_bash_paths = [
        "C:\\Program Files\\Git\\bin\\bash.exe",
        "C:\\Program Files (x86)\\Git\\bin\\bash.exe",
    ];
    for path in &git_bash_paths {
        if std::path::Path::new(path).exists() {
            if seen.insert("git-bash".to_string()) {
                shells.push(("Git Bash".to_string(), path.to_string()));
                break;
            }
        }
    }
}
