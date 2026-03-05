use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "debug-fps")]
mod redraw_debug {
    use std::sync::atomic::AtomicU64;
    pub static DIRTY_COUNT: AtomicU64 = AtomicU64::new(0);
    pub static BLINK_COUNT: AtomicU64 = AtomicU64::new(0);
    pub static ANIM_COUNT: AtomicU64 = AtomicU64::new(0);
    pub static PAUSE_COUNT: AtomicU64 = AtomicU64::new(0);
}

use alacritty_terminal::selection::SelectionType;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::draw::DrawContext;
use crate::dropdown::{DropdownElement, DropdownMenu, MenuAction, MenuEntry, MenuPosition};
use crate::gpu::GpuContext;
use crate::icons;
use crate::icons::IconManager;
use crate::layout::{Rect, TextSpec};
use crate::saved_sessions::{now_unix, SavedAuthType, SavedSession, SavedSessions};
use crate::ssh_dialog::{AuthMethod, SshDialog, SshPrefill, SshResult};
use crate::tab_bar::{TabBar, TabBarElement};
use crate::terminal_panel::{EventProxy, PanelId, TermSize, TerminalEvent, TerminalPanel};
use crate::theme::Theme;

/// Fixed blink interval — 30 fps.
const BLINK_INTERVAL: Duration = Duration::from_millis(33);

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
    ssh_dialog: Option<SshDialog>,
    saved_sessions: SavedSessions,
    cached_shells: Option<Vec<(String, String)>>,
    shell_receiver: Option<std::sync::mpsc::Receiver<Vec<(String, String)>>>,
    cursor_position: (f32, f32),
    super_pressed: bool,
    ctrl_pressed: bool,
    alt_pressed: bool,
    shift_pressed: bool,
    mouse_left_pressed: bool,
    last_click_time: Instant,
    click_count: u8,
    screenshot_pending: Option<String>,
    last_redraw: Instant,
    /// Set when new terminal content or user input arrives — forces an
    /// immediate render regardless of the cursor-blink throttle.
    dirty: bool,
    /// Window is fully occluded (hidden behind other windows) — skip all rendering.
    occluded: bool,

    // Cached scene data from the last full (dirty) redraw.
    // Reused during blink-only frames to avoid rebuilding the scene.
    cached_scene: DrawContext,
    cached_overlay: DrawContext,
    cached_scene_panel_text: Vec<TextSpec>,
    cached_scene_tab_text: Vec<TextSpec>,
    cached_overlay_text: Vec<TextSpec>,
}

impl App {
    pub fn new(event_proxy_raw: EventLoopProxy<TerminalEvent>) -> Self {
        let theme = Theme::new();
        let saved_sessions = SavedSessions::load();
        let (shell_tx, shell_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = shell_tx.send(detect_shells());
        });
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
            ssh_dialog: None,
            saved_sessions,
            cached_shells: None,
            shell_receiver: Some(shell_rx),
            cursor_position: (0.0, 0.0),
            super_pressed: false,
            ctrl_pressed: false,
            alt_pressed: false,
            shift_pressed: false,
            mouse_left_pressed: false,
            last_click_time: Instant::now(),
            click_count: 0,
            screenshot_pending: std::env::var("SCREENSHOT").ok().filter(|s| !s.is_empty()),
            last_redraw: Instant::now(),
            dirty: false,
            occluded: false,
            cached_scene: DrawContext::new(),
            cached_overlay: DrawContext::new(),
            cached_scene_panel_text: Vec::new(),
            cached_scene_tab_text: Vec::new(),
            cached_overlay_text: Vec::new(),
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
    ) -> Result<TerminalPanel, String> {
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
        let Some(gpu) = self.gpu.as_ref() else { return };
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
        let Some(gpu) = self.gpu.as_mut() else { return };
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

    /// Full redraw: rebuild scene from scratch, upload everything to GPU.
    /// Returns true if a screenshot was captured (signals auto-exit).
    fn redraw(&mut self) -> bool {
        #[cfg(feature = "debug-fps")]
        let _debug_t0 = Instant::now();
        if self.gpu.is_none() || self.tabs.is_empty() {
            return false;
        }

        let Some(gpu) = self.gpu.as_mut() else { return false };
        let scale = gpu.scale_factor;
        let cell = gpu.cell;
        let colors = gpu.colors.clone();
        let theme = &self.theme;

        // Scene: panels + tab bar — reuse cached Vecs to avoid per-frame allocation
        let mut scene = std::mem::take(&mut self.cached_scene);
        let mut scene_tab_text = std::mem::take(&mut self.cached_scene_tab_text);
        let mut scene_panel_text = std::mem::take(&mut self.cached_scene_panel_text);
        scene.clear();
        scene_tab_text.clear();
        scene_panel_text.clear();

        #[cfg(feature = "debug-fps")]
        let _debug_t1 = Instant::now();

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

        #[cfg(feature = "debug-fps")]
        let _debug_t2 = Instant::now();

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

        // Overlay: scrollbar + dropdown + SSH dialog — reuse cached Vecs
        let mut overlay = std::mem::take(&mut self.cached_overlay);
        let mut overlay_dd_text = std::mem::take(&mut self.cached_overlay_text);
        overlay.clear();
        overlay_dd_text.clear();
        panel.draw_scrollbar(&mut overlay);
        if self.dropdown.is_open() {
            self.dropdown.draw(&mut overlay, &mut overlay_dd_text, theme, scale);
        }
        let dd_bufs = self.dropdown.item_buffers();

        // SSH dialog overlay (scrim + dialog body + auth dropdown)
        let mut dialog_text_areas: Vec<glyphon::TextArea> = Vec::new();
        let mut dialog_dd_text: Vec<TextSpec> = Vec::new();
        if let Some(dialog) = &self.ssh_dialog {
            let sw = gpu.surface_config.width as f32;
            let sh = gpu.surface_config.height as f32;
            SshDialog::draw_scrim(&mut overlay, sw, sh);
            dialog.draw(&mut overlay, &mut dialog_text_areas, scale, &colors);
            if dialog.auth_dropdown().is_open() {
                dialog.auth_dropdown().draw(
                    &mut overlay,
                    &mut dialog_dd_text,
                    theme,
                    scale,
                );
            }
        }
        let auth_dd_bufs = self.ssh_dialog.as_ref()
            .map(|d| d.auth_dropdown().item_buffers())
            .unwrap_or(&[]);

        #[cfg(feature = "debug-fps")]
        let _debug_t3 = Instant::now();

        let scene_text: Vec<(&[TextSpec], &[glyphon::Buffer])> = vec![
            (&scene_tab_text, tab_bufs),
            (&scene_panel_text, panel_bufs),
        ];
        let overlay_text: Vec<(&[TextSpec], &[glyphon::Buffer])> = vec![
            (&overlay_dd_text, dd_bufs),
            (&dialog_dd_text, auth_dd_bufs),
        ];

        let screenshot = self.screenshot_pending.take();
        let took_screenshot = screenshot.is_some();
        match gpu.render_frame(
            &scene,
            &overlay,
            &scene_text,
            &overlay_text,
            dialog_text_areas,
            &self.icon_manager,
            screenshot.as_deref(),
            true, // content_changed
        ) {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let w = gpu.surface_config.width;
                let h = gpu.surface_config.height;
                gpu.resize(w, h);
            }
            Err(_) => {}
        }

        // Cache scene data for blink-only frames
        self.cached_scene = scene;
        self.cached_overlay = overlay;
        self.cached_scene_panel_text = scene_panel_text;
        self.cached_scene_tab_text = scene_tab_text;
        self.cached_overlay_text = overlay_dd_text;

        #[cfg(feature = "debug-fps")]
        let _debug_t4 = Instant::now();

        #[cfg(feature = "debug-fps")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static TOTAL_US: AtomicU64 = AtomicU64::new(0);
            static COUNT: AtomicU64 = AtomicU64::new(0);
            static LAST: AtomicU64 = AtomicU64::new(0);
            static PANEL_US: AtomicU64 = AtomicU64::new(0);
            static TAB_US: AtomicU64 = AtomicU64::new(0);
            static GPU_US: AtomicU64 = AtomicU64::new(0);
            let panel = _debug_t2.duration_since(_debug_t1).as_micros() as u64;
            let tab = _debug_t3.duration_since(_debug_t2).as_micros() as u64;
            let gpuf = _debug_t4.duration_since(_debug_t3).as_micros() as u64;
            let total = _debug_t4.duration_since(_debug_t0).as_micros() as u64;
            PANEL_US.fetch_add(panel, Ordering::Relaxed);
            TAB_US.fetch_add(tab, Ordering::Relaxed);
            GPU_US.fetch_add(gpuf, Ordering::Relaxed);
            TOTAL_US.fetch_add(total, Ordering::Relaxed);
            let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            let l = LAST.load(Ordering::Relaxed);
            if now_ms - l >= 3000 {
                LAST.store(now_ms, Ordering::Relaxed);
                let p = PANEL_US.swap(0, Ordering::Relaxed);
                let t = TAB_US.swap(0, Ordering::Relaxed);
                let g = GPU_US.swap(0, Ordering::Relaxed);
                let tot = TOTAL_US.swap(0, Ordering::Relaxed);
                let dc = COUNT.swap(0, Ordering::Relaxed).max(1);
                eprintln!("[render] frames={dc} panel={}us tab={}us gpu={}us total={}us",
                    p/dc, t/dc, g/dc, tot/dc);
            }
        }
        took_screenshot
    }

    /// Blink-only redraw: reuse cached scene, only update cursor uniform.
    /// Skips scene rebuild and GPU data uploads for minimal CPU usage.
    fn redraw_blink_only(&mut self) {
        // When the SSH dialog is open, always do a full redraw
        if self.ssh_dialog.is_some() {
            self.redraw();
            return;
        }

        if self.gpu.is_none() || self.tabs.is_empty() {
            return;
        }

        let Some(gpu) = self.gpu.as_mut() else { return };
        let colors = &gpu.colors;
        let scale = gpu.scale_factor;

        // Update only the cursor data in the cached scene
        if let Some(panel) = self.tabs.get(self.active_tab) {
            self.cached_scene.cursor = panel.cursor_data(colors, scale);
        }

        let panel_bufs = self.tabs.get(self.active_tab)
            .map(|p| p.buffers())
            .unwrap_or(&[]);
        let tab_bufs = self.tab_bar.tab_buffers();
        let dd_bufs = self.dropdown.item_buffers();

        let scene_text: Vec<(&[TextSpec], &[glyphon::Buffer])> = vec![
            (&self.cached_scene_tab_text, tab_bufs),
            (&self.cached_scene_panel_text, panel_bufs),
        ];
        let overlay_text: Vec<(&[TextSpec], &[glyphon::Buffer])> = vec![
            (&self.cached_overlay_text, dd_bufs),
        ];

        match gpu.render_frame(
            &self.cached_scene,
            &self.cached_overlay,
            &scene_text,
            &overlay_text,
            Vec::new(),
            &self.icon_manager,
            None,
            false, // content_changed = false
        ) {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let w = gpu.surface_config.width;
                let h = gpu.surface_config.height;
                gpu.resize(w, h);
            }
            Err(_) => {}
        }
    }

    fn request_redraw(&mut self) {
        self.dirty = true;
        #[cfg(feature = "debug-fps")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static RR_COUNT: AtomicU64 = AtomicU64::new(0);
            static RR_LAST: AtomicU64 = AtomicU64::new(0);
            let c = RR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            let l = RR_LAST.load(Ordering::Relaxed);
            if now - l >= 3000 { RR_LAST.store(now, Ordering::Relaxed); eprintln!("[request_redraw] total: {c}"); }
        }
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
        let Some(gpu) = self.gpu.as_ref() else { return };
        // When shell is None, alacritty_terminal uses its own default_shell_command
        // which launches a proper login shell via /usr/bin/login on macOS.
        // This ensures ~/.zprofile is sourced and Homebrew PATH is available.
        let panel = match self.create_terminal_panel(gpu, shell, Vec::new()) {
            Ok(p) => p,
            Err(error) => {
                let (panel_id, vp, event_proxy) = self.new_panel_params(gpu);
                let size = TermSize::new(vp.cols, vp.rows);
                TerminalPanel::new_error(panel_id, size, event_proxy, &error)
            }
        };
        self.add_tab(panel);
    }

    fn new_tab_ssh(&mut self, result: SshResult) {
        self.connect_ssh(result.to_ssh_config());
    }

    fn connect_ssh(&mut self, config: crate::ssh::SshConfig) {
        let Some(gpu) = self.gpu.as_ref() else { return };
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
        // Populate cache from background thread if not yet available
        if self.cached_shells.is_none() {
            if let Some(rx) = self.shell_receiver.take() {
                // recv() blocks only if the thread hasn't finished yet;
                // by the time the user clicks "+", it's almost certainly done.
                if let Ok(shells) = rx.recv() {
                    self.cached_shells = Some(shells);
                }
            }
        }
        let shells = self.cached_shells.as_deref().unwrap_or(&[]);
        let saved = &self.saved_sessions.sessions;

        let Some(gpu) = self.gpu.as_mut() else { return };
        let scale = gpu.scale_factor;
        let surface_w = gpu.surface_config.width as f32;
        let surface_h = gpu.surface_config.height as f32;

        let mut entries: Vec<MenuEntry> = shells
            .iter()
            .map(|(label, path)| MenuEntry::item(label, MenuAction::NewShell(path.clone())))
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
            MenuPosition::BelowAnchor(anchor),
            Some(280.0),
            scale,
            surface_w,
            surface_h,
            &mut gpu.font_system,
            &self.theme.dropdown,
        );
    }

    fn open_context_menu(&mut self, x: f32, y: f32) {
        let has_selection = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|p| p.has_selection());

        let mut entries = Vec::new();
        if has_selection {
            entries.push(MenuEntry::item("Copy", MenuAction::Copy));
        }
        entries.push(MenuEntry::item("Paste", MenuAction::Paste));

        let Some(gpu) = self.gpu.as_mut() else { return };
        let scale = gpu.scale_factor;
        let surface_w = gpu.surface_config.width as f32;
        let surface_h = gpu.surface_config.height as f32;

        self.dropdown.open(
            entries,
            MenuPosition::AtPoint(x, y),
            None,
            scale,
            surface_w,
            surface_h,
            &mut gpu.font_system,
            &self.theme.dropdown,
        );
    }

    fn open_ssh_dialog(&mut self, prefill: Option<SshPrefill>) {
        if self.ssh_dialog.is_some() {
            return; // already open
        }
        let Some(gpu) = self.gpu.as_mut() else { return };
        let scale = gpu.scale_factor;
        let sw = gpu.surface_config.width as f32;
        let sh = gpu.surface_config.height as f32;
        self.ssh_dialog = Some(SshDialog::new(
            scale,
            &self.theme,
            &mut gpu.font_system,
            prefill.as_ref(),
            sw,
            sh,
        ));
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
            password: match result.auth_method {
                AuthMethod::Password if !result.password.is_empty() => {
                    Some(result.password.clone())
                }
                _ => None,
            },
            last_used: now_unix(),
        };
        self.saved_sessions.upsert(saved);
    }

    fn execute_menu_action(&mut self, action: &MenuAction) {
        match action {
            MenuAction::NewShell(shell_path) => {
                self.new_tab(Some(shell_path.clone()));
            }
            MenuAction::OpenSshDialog => {
                self.open_ssh_dialog(None);
            }
            MenuAction::ConnectSavedSession(key) => {
                if let Some(session) = self.saved_sessions.find_by_key(key) {
                    let config = crate::ssh::SshConfig {
                        host: session.host.clone(),
                        port: session.port,
                        username: session.username.clone(),
                        auth: match &session.auth_type {
                            SavedAuthType::Password => {
                                crate::ssh::SshAuth::Password(
                                    session.password.clone().unwrap_or_default(),
                                )
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
            MenuAction::Copy => {
                if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                    if let Some(text) = panel.selection_to_string() {
                        if let Ok(mut clip) = arboard::Clipboard::new() {
                            let _ = clip.set_text(text);
                        }
                        panel.clear_selection();
                    }
                }
            }
            MenuAction::Paste => {
                if let Ok(mut clip) = arboard::Clipboard::new()
                    && let Ok(text) = clip.get_text()
                    && let Some(panel) = self.tabs.get_mut(self.active_tab)
                {
                    panel.notify_input();
                    panel.write_to_pty(text.into_bytes());
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
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600))
            .with_visible(false);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(_) => {
                event_loop.exit();
                return;
            }
        };
        let gpu = match GpuContext::new(window.clone(), self.theme.colors.clone()) {
            Some(g) => g,
            None => {
                event_loop.exit();
                return;
            }
        };

        self.window = Some(window.clone());
        self.gpu = Some(gpu);

        // Set up native menu bar after winit initialization
        crate::menu::setup_native_menu();

        self.new_tab(None);

        // Render the first frame before showing the window to avoid a blank flash
        self.redraw();
        window.set_visible(true);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TerminalEvent) {
        match event {
            TerminalEvent::Wakeup => {
                self.dirty = true;
                self.request_redraw();
                #[cfg(feature = "debug-fps")]
                {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static COUNT: AtomicU64 = AtomicU64::new(0);
                    static LAST: AtomicU64 = AtomicU64::new(0);
                    let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    let last = LAST.load(Ordering::Relaxed);
                    if now - last >= 3000 { LAST.store(now, Ordering::Relaxed); eprintln!("[wakeup] total: {c}"); }
                }
            }
            TerminalEvent::Title(panel_id, title) => {
                #[cfg(feature = "debug-fps")]
                {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static COUNT: AtomicU64 = AtomicU64::new(0);
                    static LAST: AtomicU64 = AtomicU64::new(0);
                    let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    let l = LAST.load(Ordering::Relaxed);
                    if now - l >= 3000 { LAST.store(now, Ordering::Relaxed); eprintln!("[title] total: {c}"); }
                }
                if let Some(panel) = self.tabs.iter_mut().find(|p| p.id() == panel_id) {
                    panel.set_title(title);
                }
                self.sync_tab_state();
            }
            TerminalEvent::Exit(panel_id) => {
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
        // Ignore events for unknown windows
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
                    // Recenter SSH dialog on resize
                    if let Some(dialog) = &mut self.ssh_dialog {
                        let scale = gpu.scale_factor;
                        let sw = new_size.width as f32;
                        let sh = new_size.height as f32;
                        dialog.compute_layout_centered(scale, sw, sh, &mut gpu.font_system);
                    }
                }
                self.sync_tab_state();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dropdown.close();
                if let Some(gpu) = &mut self.gpu {
                    gpu.scale_factor = scale_factor as f32;
                    // Recenter SSH dialog on scale change
                    if let Some(dialog) = &mut self.ssh_dialog {
                        let sw = gpu.surface_config.width as f32;
                        let sh = gpu.surface_config.height as f32;
                        dialog.compute_layout_centered(
                            scale_factor as f32, sw, sh, &mut gpu.font_system,
                        );
                    }
                }
                self.sync_tab_state();
            }

            WindowEvent::Occluded(is_occluded) => {
                self.occluded = is_occluded;
                if !is_occluded {
                    self.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => {
                #[cfg(feature = "debug-fps")]
                {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    use crate::app::redraw_debug::*;
                    static COUNT: AtomicU64 = AtomicU64::new(0);
                    static LAST: AtomicU64 = AtomicU64::new(0);
                    if self.dirty { DIRTY_COUNT.fetch_add(1, Ordering::Relaxed); }
                    let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    let l = LAST.load(Ordering::Relaxed);
                    if now - l >= 3000 {
                        LAST.store(now, Ordering::Relaxed);
                        let d = DIRTY_COUNT.swap(0, Ordering::Relaxed);
                        let b = BLINK_COUNT.swap(0, Ordering::Relaxed);
                        let a = ANIM_COUNT.swap(0, Ordering::Relaxed);
                        let p = PAUSE_COUNT.swap(0, Ordering::Relaxed);
                        let n = COUNT.swap(0, Ordering::Relaxed).max(1);
                        eprintln!("[redraw_req] total={c} dirty={d} blink={b} anim={a} pause={p} (per {n})");
                    }
                }
                if self.occluded {
                    return;
                }

                let now = Instant::now();
                let elapsed = now.duration_since(self.last_redraw);

                // New terminal content or user input — full render immediately
                if self.dirty {
                    self.dirty = false;
                    self.redraw();
                    self.last_redraw = Instant::now();
                    // Keep the blink loop going if cursor is visible, but use a timer
                    if self.tabs.get(self.active_tab).is_some_and(|p| p.cursor_visible()) {
                        let interval = if self.tabs.get(self.active_tab)
                            .is_some_and(|p| p.cursor_animating() || p.is_smooth_scrolling())
                        {
                            BLINK_INTERVAL // ~30fps for active animations
                        } else {
                            // During blink pause (500ms after input), sleep until
                            // pause expires instead of rendering 30fps of identical
                            // solid cursor.
                            let input_age = self.tabs.get(self.active_tab)
                                .map(|p| p.last_input_time().elapsed())
                                .unwrap_or(Duration::from_secs(1));
                            if input_age < Duration::from_millis(500) {
                                let remaining = Duration::from_millis(500) - input_age;
                                event_loop.set_control_flow(ControlFlow::WaitUntil(
                                    Instant::now() + remaining,
                                ));
                                return;
                            }
                            BLINK_INTERVAL
                        };
                        let wait_until = self.last_redraw + interval;
                        event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
                    }
                    return;
                }

                if let Some(panel) = self.tabs.get(self.active_tab) {
                    if panel.cursor_visible() {
                        if panel.cursor_animating() || panel.is_smooth_scrolling() {
                            // Active animation — full redraw at ~60fps
                            #[cfg(feature = "debug-fps")]
                            redraw_debug::ANIM_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            self.redraw();
                            self.last_redraw = Instant::now();
                            let wait_until = self.last_redraw + BLINK_INTERVAL;
                            event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
                        } else {
                            // Check blink pause: during 500ms after input, cursor
                            // is solid — no need to render at all.
                            let input_age = panel.last_input_time().elapsed();
                            if input_age < Duration::from_millis(500) {
                                #[cfg(feature = "debug-fps")]
                                redraw_debug::PAUSE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let remaining = Duration::from_millis(500) - input_age;
                                event_loop.set_control_flow(ControlFlow::WaitUntil(
                                    Instant::now() + remaining,
                                ));
                                return;
                            }

                            if elapsed >= BLINK_INTERVAL {
                                // Idle cursor blink — use blink-only fast path
                                #[cfg(feature = "debug-fps")]
                                redraw_debug::BLINK_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                self.redraw_blink_only();
                                self.last_redraw = Instant::now();
                                let wait_until = self.last_redraw + BLINK_INTERVAL;
                                event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
                            } else {
                                // Too soon for idle blink — sleep until next frame
                                let wait_until = self.last_redraw + BLINK_INTERVAL;
                                event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
                            }
                        }
                    } else {
                        // Cursor hidden — just render (no continuous redraw)
                        self.redraw();
                        self.last_redraw = Instant::now();
                    }
                } else {
                    self.redraw();
                    self.last_redraw = Instant::now();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // position is already in physical pixels
                self.cursor_position = (position.x as f32, position.y as f32);
                let (cx, cy) = self.cursor_position;

                // SSH dialog intercepts mouse move when open
                if let Some(dialog) = &mut self.ssh_dialog {
                    let cursor = dialog.handle_mouse_move(cx, cy);
                    if let Some(window) = &self.window {
                        window.set_cursor(cursor);
                    }
                    self.request_redraw();
                    return;
                }

                // Dropdown hover takes priority when open
                if self.dropdown.is_open() {
                    let hover = self.dropdown.hit_test(cx, cy);
                    if self.dropdown.set_hover(hover) {
                        self.request_redraw();
                    }
                    return;
                }

                // Drag scrollbar or selection
                if self.mouse_left_pressed {
                    if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                        if panel.is_scrollbar_dragging() {
                            panel.update_scrollbar_drag(cy);
                            self.request_redraw();
                        } else if let Some((point, side)) = panel.pixel_to_point(cx, cy) {
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

                // Set cursor icon
                if let Some(window) = &self.window {
                    let panel = self.tabs.get(self.active_tab);
                    let dragging_scrollbar = panel.as_ref().is_some_and(|p| p.is_scrollbar_dragging());
                    let on_scrollbar = panel.as_ref().is_some_and(|p| p.is_in_scrollbar_area(cx, cy));
                    let in_content = panel.is_some_and(|p| p.is_in_content_area(cx, cy));
                    let icon = if dragging_scrollbar {
                        CursorIcon::Grabbing
                    } else if on_scrollbar {
                        CursorIcon::Grab
                    } else if in_content {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Default
                    };
                    window.set_cursor(icon);
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

                // SSH dialog intercepts all clicks when open
                if self.ssh_dialog.is_some() {
                    let Some(gpu) = self.gpu.as_mut() else { return };
                    let dropdown_theme = self.theme.dropdown.clone();
                    let dialog = self.ssh_dialog.as_mut().unwrap();
                    match dialog.handle_mouse_click(
                        cx, cy,
                        &mut gpu.font_system,
                        &dropdown_theme,
                    ) {
                        Ok(None) => {}
                        Ok(Some(result)) => {
                            self.ssh_dialog = None;
                            self.save_ssh_session(&result);
                            self.new_tab_ssh(result);
                        }
                        Err(()) => {
                            self.ssh_dialog = None;
                        }
                    }
                    self.request_redraw();
                    return;
                }

                // Dropdown intercepts all clicks when open
                if self.dropdown.is_open() {
                    match self.dropdown.hit_test(cx, cy) {
                        DropdownElement::Item(idx) => {
                            let action = self.dropdown.action_for(idx).cloned();
                            self.dropdown.close();
                            if let Some(action) = action {
                                self.execute_menu_action(&action);
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
                    // Try scrollbar drag first
                    if panel.try_start_scrollbar_drag(cx, cy) {
                        self.mouse_left_pressed = true;
                        self.request_redraw();
                    } else if let Some((point, side)) = panel.pixel_to_point(cx, cy) {
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
                if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                    panel.stop_scrollbar_drag();
                }
                self.mouse_left_pressed = false;
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if self.ssh_dialog.is_some() {
                    return; // Dialog absorbs right-clicks
                }
                let (cx, cy) = self.cursor_position;
                if self.dropdown.is_open() {
                    self.dropdown.close();
                }
                self.open_context_menu(cx, cy);
                self.request_redraw();
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.super_pressed = new_modifiers.state().super_key();
                self.ctrl_pressed = new_modifiers.state().control_key();
                self.alt_pressed = new_modifiers.state().alt_key();
                self.shift_pressed = new_modifiers.state().shift_key();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // SSH dialog intercepts all keyboard input when open
                if self.ssh_dialog.is_some() {
                    let Some(gpu) = self.gpu.as_mut() else { return };
                    let dialog = self.ssh_dialog.as_mut().unwrap();
                    let shift = self.shift_pressed;
                    let super_p = self.super_pressed;
                    let ctrl_p = self.ctrl_pressed;
                    match dialog.handle_key_event(
                        &event,
                        &mut gpu.font_system,
                        super_p,
                        ctrl_p,
                        shift,
                    ) {
                        Ok(None) => {}
                        Ok(Some(result)) => {
                            self.ssh_dialog = None;
                            self.save_ssh_session(&result);
                            self.new_tab_ssh(result);
                        }
                        Err(()) => {
                            self.ssh_dialog = None;
                        }
                    }
                    self.request_redraw();
                    return;
                }

                // Close dropdown on Escape
                if self.dropdown.is_open()
                    && event.state == ElementState::Pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                {
                    self.dropdown.close();
                    self.request_redraw();
                    return;
                }

                // macOS: Cmd+key shortcuts (copy, paste, screenshot).
                // Block ALL Cmd+key from reaching the terminal.
                #[cfg(target_os = "macos")]
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
                                && let Some(panel) = self.tabs.get_mut(self.active_tab)
                            {
                                panel.notify_input();
                                panel.write_to_pty(text.into_bytes());
                            }
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyS) => {
                            self.screenshot_pending =
                                Some("/tmp/pfauterminal_screenshot.png".to_string());
                            self.request_redraw();
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyT) => {
                            self.new_tab(None);
                            self.request_redraw();
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyW) => {
                            if self.tabs.len() <= 1 {
                                event_loop.exit();
                            } else {
                                self.close_tab(self.active_tab);
                            }
                            return;
                        }
                        _ => {}
                    }
                    // Don't pass Cmd+key combos to the terminal
                    return;
                }

                // Windows/Linux: Ctrl+key clipboard shortcuts.
                // Only intercept copy and paste — all other Ctrl+key combos
                // must reach the terminal as control characters.
                #[cfg(not(target_os = "macos"))]
                if event.state == ElementState::Pressed && self.ctrl_pressed {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyC) => {
                            // Ctrl+C with active selection → copy to clipboard.
                            // Without selection → fall through to send SIGINT (0x03).
                            if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                                if panel.has_selection() {
                                    if let Some(text) = panel.selection_to_string() {
                                        if let Ok(mut clip) = arboard::Clipboard::new() {
                                            let _ = clip.set_text(text);
                                        }
                                    }
                                    panel.clear_selection();
                                    self.request_redraw();
                                    return;
                                }
                            }
                        }
                        PhysicalKey::Code(KeyCode::KeyV) => {
                            if let Ok(mut clip) = arboard::Clipboard::new()
                                && let Ok(text) = clip.get_text()
                                && let Some(panel) = self.tabs.get_mut(self.active_tab)
                            {
                                panel.notify_input();
                                panel.write_to_pty(text.into_bytes());
                            }
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyT) => {
                            self.new_tab(None);
                            self.request_redraw();
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyW) => {
                            if self.tabs.len() <= 1 {
                                event_loop.exit();
                            } else {
                                self.close_tab(self.active_tab);
                            }
                            return;
                        }
                        _ => {}
                    }
                }

                if let Some(panel) = self.tabs.get_mut(self.active_tab) {
                    panel.handle_key(&event, self.ctrl_pressed, self.alt_pressed);
                    self.dirty = true;
                    self.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if self.ssh_dialog.is_some() {
                    return; // Dialog absorbs scroll events
                }
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

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        // When woken by WaitUntil timer, render directly instead of going
        // through request_redraw → RedrawRequested to avoid macOS event
        // dispatch overhead (~600us per wakeup).
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if self.occluded {
                return;
            }
            if self.dirty {
                // Content changed while sleeping — do a full redraw via
                // the standard path.
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            // Extract panel state before mutable borrows
            let panel_state = self.tabs.get(self.active_tab).map(|p| {
                (p.cursor_visible(), p.cursor_animating(), p.is_smooth_scrolling(), p.last_input_time())
            });
            if let Some((cursor_vis, animating, scrolling, last_input)) = panel_state {
                if cursor_vis {
                    if animating || scrolling {
                        // Active animation — full redraw at ~60fps
                        self.redraw();
                        self.last_redraw = Instant::now();
                        let wait_until = self.last_redraw + BLINK_INTERVAL;
                        event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
                    } else {
                        // Check blink pause
                        let input_age = last_input.elapsed();
                        if input_age < Duration::from_millis(500) {
                            let remaining = Duration::from_millis(500) - input_age;
                            event_loop.set_control_flow(ControlFlow::WaitUntil(
                                Instant::now() + remaining,
                            ));
                            return;
                        }
                        // Blink-only render — skip event dispatch overhead
                        self.redraw_blink_only();
                        self.last_redraw = Instant::now();
                        let wait_until = self.last_redraw + BLINK_INTERVAL;
                        event_loop.set_control_flow(ControlFlow::WaitUntil(wait_until));
                    }
                }
            }
        }
    }
}

/// Return the user's default login shell as (name, path).
fn detect_default_shell() -> Option<(String, String)> {
    #[cfg(not(windows))]
    {
        if let Ok(shell_path) = std::env::var("SHELL") {
            let path = std::path::Path::new(&shell_path);
            if path.exists() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("shell")
                    .to_string();
                return Some((name, shell_path));
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(comspec) = std::env::var("ComSpec") {
            if std::path::Path::new(&comspec).exists() {
                return Some(("cmd".to_string(), comspec));
            }
        }
    }
    None
}

/// Detect available shells on the system. Returns (display_label, full_path) pairs.
/// The user's default login shell is placed first with a "Default (...)" label.
fn detect_shells() -> Vec<(String, String)> {
    let mut shells = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Insert the user's default login shell at the top
    if let Some((name, path)) = detect_default_shell() {
        seen.insert(name.clone());
        shells.push((format!("Default ({name})"), path));
    }

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
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        if let Ok(output) = std::process::Command::new("where")
            .arg("pwsh")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
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
