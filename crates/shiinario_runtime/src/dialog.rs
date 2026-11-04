//! MessageBox button IDs shared by native hosts and explicit trace simulation.
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use anyhow::ensure;

pub(crate) fn buttons(flags: u32) -> Result<&'static [(u32, &'static str)]> {
    match flags & 15 {
        0 => Ok(&[(1, "OK")]),
        1 => Ok(&[(1, "OK"), (2, "Cancel")]),
        4 => Ok(&[(6, "Yes"), (7, "No")]),
        _ => anyhow::bail!("unsupported message box button flags {flags:#x}"),
    }
}

pub(crate) fn default_reply(flags: u32) -> Result<u32> {
    let buttons = buttons(flags)?;
    buttons
        .get(((flags >> 8) & 3) as usize)
        .map(|b| b.0)
        .ok_or_else(|| anyhow::anyhow!("message box default button is outside button list"))
}

#[cfg(target_os = "windows")]
pub(crate) fn show(text: &str, title: &str, flags: u32) -> Result<u32> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            title: *const u16,
            flags: u32,
        ) -> i32;
    }
    let text: Vec<_> = text.encode_utf16().chain([0]).collect();
    let title: Vec<_> = title.encode_utf16().chain([0]).collect();
    let value = unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags) };
    ensure!(
        value > 0,
        "message box failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(value as u32)
}

#[cfg(all(
    not(target_arch = "wasm32"),
    not(target_os = "windows"),
    not(target_os = "macos")
))]
pub(crate) fn show(text: &str, title: &str, flags: u32) -> Result<u32> {
    use anyhow::Context;
    use std::process::{Command, Stdio};
    let choices = buttons(flags)?;
    ensure!(
        (flags >> 8) & 3 == 0,
        "this dialog host requires the first default button"
    );
    let kind = match flags & 15 {
        1 => "--warningcontinuecancel",
        4 => "--yesno",
        _ => "--msgbox",
    };
    let result = Command::new("kdialog")
        .args(["--title", title, kind, text])
        .stdin(Stdio::null())
        .status();
    let status = match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut command = Command::new("zenity");
            command.args(["--title", title, "--text", text, "--no-markup"]);
            if choices.len() == 1 {
                command.arg("--info");
            } else {
                command.args([
                    "--question",
                    "--ok-label",
                    choices[0].1,
                    "--cancel-label",
                    choices[1].1,
                ]);
            }
            command
                .stdin(Stdio::null())
                .status()
                .context("cannot open a message box; install kdialog or zenity")?
        }
        result => result.context("cannot open the message box")?,
    };
    match status.code() {
        Some(0) => Ok(choices[0].0),
        Some(1) if choices.len() == 2 => Ok(choices[1].0),
        _ => anyhow::bail!("message box process failed: {status}"),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn show(text: &str, title: &str, flags: u32) -> Result<u32> {
    use anyhow::Context;
    let choices = buttons(flags)?;
    let default_id = default_reply(flags)?;
    let default = choices.iter().find(|b| b.0 == default_id).unwrap().1;
    let script = r#"on run argv
set buttonNames to items 4 thru -1 of argv
try
set result to display dialog (item 1 of argv) with title (item 2 of argv) buttons buttonNames default button (item 3 of argv)
return button returned of result
on error number -128
return item -1 of buttonNames
end try
end run"#;
    let names: Vec<&str> = choices.iter().map(|b| b.1).collect();
    let result = std::process::Command::new("osascript")
        .args(["-e", script, "--", text, title, default])
        .args(&names)
        .output()
        .context("cannot open the message box")?;
    ensure!(result.status.success(), "message box process failed");
    let label = String::from_utf8(result.stdout)?;
    choices
        .iter()
        .find(|b| b.1 == label.trim())
        .map(|b| b.0)
        .context("invalid message box reply")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_button_ids_and_default_buttons() {
        assert_eq!(default_reply(48).unwrap(), 1);
        assert_eq!(default_reply(49).unwrap(), 1);
        assert_eq!(default_reply(4).unwrap(), 6);
        assert_eq!(default_reply(0x104).unwrap(), 7);
        assert_eq!(default_reply(0x101).unwrap(), 2);
        assert!(default_reply(0x200).is_err());
    }
}
