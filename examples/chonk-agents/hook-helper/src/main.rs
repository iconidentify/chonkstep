fn main() {
    // An observer never controls a provider operation, including on local failure.
    chonk_agent_hook::install_deadline();
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(chonk_agent_hook::run);
}
