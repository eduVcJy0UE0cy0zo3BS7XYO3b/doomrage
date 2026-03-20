#[unsafe(no_mangle)]
pub extern "C" fn run(x: f64) -> f64 {
    x.sqrt()
}
