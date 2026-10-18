//! Sprint desktop GUI.
//! See `ARCHITECTURE.md` for the public contract.

use std::path::PathBuf;

/// Options for launching the GUI.
#[derive(Clone, Debug, Default)]
pub struct GuiOptions {
    /// G-code file to load at startup.
    pub file: Option<PathBuf>,
    /// Connect to the built-in virtual printer at startup.
    pub simulator: bool,
}

/// Launch the GUI and block until the window closes.
pub fn run(options: GuiOptions) -> anyhow::Result<()> {
    let _ = options;
    anyhow::bail!("the Sprint GUI is not implemented yet")
}
