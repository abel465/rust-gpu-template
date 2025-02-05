use crate::{
    shader::{maybe_watch, CompiledShaderModules},
    user_event::UserEvent,
};
use egui_winit::winit::event_loop::EventLoop;
use structopt::StructOpt;

mod app;
mod bind_group_buffer;
mod context;
mod controller;
mod fps_counter;
mod render_pass;
mod shader;
mod ui;
mod user_event;

const TITLE: &str = "runner";

#[derive(StructOpt, Clone, Copy)]
#[structopt(name = TITLE)]
pub struct Options {
    // Default to true after the following is fixed
    // https://github.com/gfx-rs/wgpu/issues/5128
    #[structopt(long)]
    validate_spirv: bool,

    /// Starts in debug mode and with speed set to 0
    #[structopt(short, long)]
    debug: bool,
}

pub fn main() {
    let options = Options::from_args();

    env_logger::init();

    let event_loop = EventLoop::with_user_event().build().unwrap();

    // Build the shader before we pop open a window, since it might take a while.
    let initial_shader = maybe_watch(&options, {
        let proxy = event_loop.create_proxy();
        Some(Box::new(move |res| {
            match proxy.send_event(UserEvent::NewModule(res)) {
                Ok(it) => it,
                // ShaderModuleDescriptor is not `Debug`, so can't use unwrap/expect
                Err(_err) => panic!("Event loop dead"),
            }
        }))
    });

    run(options, event_loop, initial_shader);
}

fn run(
    options: Options,
    event_loop: EventLoop<UserEvent>,
    compiled_shader_modules: CompiledShaderModules,
) {
    let mut app = app::App::new(event_loop.create_proxy(), compiled_shader_modules, options);
    if let Result::Err(e) = event_loop.run_app(&mut app) {
        eprintln!("Event loop Error: {e}")
    }
}
