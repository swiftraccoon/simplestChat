//! Reruns, and so recompiles the server, whenever anything under `migrations/`
//! changes. `sqlx::migrate!` embeds each migration with `include_str!`, which
//! tracks only the files the macro saw: on the stable toolchain a migration
//! that appears later would otherwise leave the embedded set stale.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
