#[unsafe(no_mangle)]
pub extern "C" fn run(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}
