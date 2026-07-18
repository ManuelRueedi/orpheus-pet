// Captures the text currently highlighted in the foreground application by
// simulating Ctrl+C and reading the clipboard — the standard trick, since
// Windows has no universal "get selection" API. The user's clipboard text is
// restored afterwards. If the foreground app produced nothing (no selection,
// or an elevated window that ignores injected input), we fall back to the
// existing clipboard text so "Ctrl+C, then hotkey" also works.

#[cfg(windows)]
pub fn capture_selection() -> String {
    use std::{thread::sleep, time::Duration};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    fn key_event(vk: u16, up: bool) -> INPUT {
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT {
            wVk: vk,
            wScan: 0,
            dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
            time: 0,
            dwExtraInfo: 0,
        };
        input
    }

    fn send(events: &[INPUT]) {
        unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
    }

    fn is_down(vk: u16) -> bool {
        (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
    }

    let mut clip = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let previous = clip.get_text().unwrap_or_default();
    let _ = clip.set_text(String::new());

    // The user is still physically holding the hotkey's modifiers; release
    // them first or the focused app sees Ctrl+Alt+C instead of Ctrl+C.
    let mods = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN];
    let release: Vec<INPUT> = mods
        .iter()
        .filter(|&&m| is_down(m))
        .map(|&m| key_event(m, true))
        .collect();
    if !release.is_empty() {
        send(&release);
        sleep(Duration::from_millis(50));
    }

    // Ctrl+C ('C' = 0x43)
    send(&[
        key_event(VK_CONTROL, false),
        key_event(0x43, false),
        key_event(0x43, true),
        key_event(VK_CONTROL, true),
    ]);

    // Apps copy asynchronously — poll instead of one fixed sleep.
    let mut captured = String::new();
    for _ in 0..20 {
        sleep(Duration::from_millis(40));
        if let Ok(t) = clip.get_text() {
            if !t.is_empty() {
                captured = t;
                break;
            }
        }
    }

    // Put the user's clipboard back the way it was.
    if !previous.is_empty() {
        let _ = clip.set_text(previous.clone());
    }

    if captured.is_empty() {
        previous
    } else {
        captured
    }
}

#[cfg(not(windows))]
pub fn capture_selection() -> String {
    String::new()
}
