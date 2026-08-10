//! Production Linux entry point for the unprivileged package broker.

fn main() {
    if pkg_installer::run_linux_broker_from_activation().is_err() {
        eprintln!("managed package service failed");
        std::process::exit(1);
    }
}
