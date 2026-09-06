pub fn cpu_count() -> usize {
    // We are going to be IO bound most of the time.
    // Reducing the number of threads really helps keep memory usage down while not making a huge
    // difference on the overall performance.
    num_cpus::get().clamp(1, 6)
}
