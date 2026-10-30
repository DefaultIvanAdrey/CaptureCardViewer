mod app;
mod util;

fn main() {
    console_error_panic_hook::set_once();
    app::App::mount();
}
