#[unsafe(no_mangle)]
pub extern "C" fn run(a: f64, b: f64) -> f64 {
    if b == 0.0 { f64::NAN } else { a / b }
}
