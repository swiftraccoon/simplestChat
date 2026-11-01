#![deny(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

fn main() -> anyhow::Result<()> {
    simplestchat::app::main()
}
