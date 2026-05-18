use std::process::Command as StdCommand;

use tokio::process::Command as TokioCommand;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub(crate) fn configure_no_window(command: &mut StdCommand) -> &mut StdCommand {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

pub(crate) fn configure_tokio_no_window(command: &mut TokioCommand) -> &mut TokioCommand {
    #[cfg(target_os = "windows")]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
