#![cfg_attr(
    all(feature = "desktop", not(debug_assertions)),
    windows_subsystem = "windows"
)]
fn main() {
    #[cfg(feature = "desktop")]
    {
        if std::env::args().any(|argument| argument == vela_lib::UNINSTALL_CLEANUP_ARG) {
            std::process::exit(vela_lib::uninstall_cleanup());
        }
        // What another machine's Vela runs here over SSH.
        #[cfg(target_os = "macos")]
        if std::env::args().skip(1).eq(["rpc-stdio"]) {
            std::process::exit(vela_lib::server::app_rpc_stdio());
        }
        vela_lib::run();
    }
    #[cfg(all(not(feature = "desktop"), unix))]
    vela_lib::server::main();
    #[cfg(all(not(feature = "desktop"), not(unix)))]
    {
        eprintln!("vela-server 不支持此系统");
        std::process::exit(1);
    }
}
