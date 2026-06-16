//! Fallback "print the commands" backend — used when no multiplexer is
//! available (or `--terminal print`). Prints each agent's command for the
//! user to paste into separate terminals manually.

use anyhow::Result;

use super::{LaunchSession, Pane, TerminalBackend};

pub struct Print;

impl TerminalBackend for Print {
    fn name(&self) -> &'static str {
        "print"
    }

    fn launch(&self, panes: &[Pane], _session: &LaunchSession) -> Result<()> {
        launch_print(panes)
    }
}

fn launch_print(panes: &[Pane]) -> Result<()> {
    println!("\nNo terminal multiplexer detected (wt.exe / tmux).");
    println!("Run each of these in its own terminal:\n");
    for p in panes {
        println!("# {} ({})", p.title, p.platform);
        println!("cd {} && {}\n", p.cwd, p.cmd);
    }
    Ok(())
}
