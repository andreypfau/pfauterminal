mod app;
mod colors;
mod draw;
mod dropdown;
mod font;
mod gpu;
mod icons;
mod layout;
mod saved_sessions;
mod ssh;
mod ssh_dialog;
mod tab_bar;
mod terminal_panel;
mod theme;
mod widgets;

use winit::event_loop::EventLoop;

use app::App;
use terminal_panel::TerminalEvent;

fn main() {
    env_logger::init();

    let event_loop = EventLoop::<TerminalEvent>::with_user_event()
        .build()
        .expect("create event loop");

    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);

    event_loop.run_app(&mut app).expect("run event loop");
}
