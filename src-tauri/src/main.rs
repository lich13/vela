#![cfg_attr(
    all(feature = "desktop", not(debug_assertions)),
    windows_subsystem = "windows"
)]
fn main() {
    #[cfg(feature = "desktop")]
    vela_lib::run();
    #[cfg(all(not(feature = "desktop"), unix))]
    vela_lib::server::main();
    #[cfg(all(not(feature = "desktop"), not(unix)))]
    {
        eprintln!("vela-server 不支持此系统");
        std::process::exit(1);
    }
}
