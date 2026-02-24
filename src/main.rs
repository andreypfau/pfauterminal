mod app;
mod colors;
mod font;
mod gpu;
mod icons;
mod layout;
mod panel;
mod panels;
mod tab_bar;
mod terminal;
mod workspace;

use winit::event_loop::EventLoop;

use app::App;
use terminal::TerminalEvent;

fn main() {
    env_logger::init();

    let event_loop = EventLoop::<TerminalEvent>::with_user_event()
        .build()
        .expect("create event loop");

    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);

    event_loop.run_app(&mut app).expect("run event loop");
}
