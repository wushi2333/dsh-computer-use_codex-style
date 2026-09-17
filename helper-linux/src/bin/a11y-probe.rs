//! Print the accessibility bus address the desktop currently advertises.
//!
//! This is the shared library's own AT-SPI discovery path -- the same
//! `AccessibilityConnection::new()` the helper uses -- exposed as a binary so the gated
//! suites can assert on what the **desktop** resolves to, without depending on the test
//! process having a usable session bus themselves (a nested session may not).
//!
//! Prints the address on stdout and exits 0; a non-zero exit means the address could not be
//! obtained, which is itself a result the caller may want to assert on.

use dsh_computer_use::atspi_tree;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match atspi_tree::accessibility_bus_address().await {
        Ok(address) => println!("{address}"),
        Err(error) => {
            eprintln!("could not obtain the accessibility bus address: {error:#}");
            std::process::exit(1);
        }
    }
}
