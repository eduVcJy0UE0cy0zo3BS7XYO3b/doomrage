#[unsafe(no_mangle)]
pub extern "C" fn run(a: f64, b: f64) -> f64 {
    a - b
}
