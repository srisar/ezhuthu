//! Tracks whether keyboard focus is in a password field, so Tamil mode never
//! composes (shows, caches or transliterates) what the user types there.
//!
//! A dedicated thread listens for focus changes and asks the focused object
//! for its MSAA state: Win32 `ES_PASSWORD` edits and browser password inputs
//! report `STATE_SYSTEM_PROTECTED`. The query is cross-process COM and may be
//! slow if the target app hangs, so it runs here, never on the UI thread
//! that serves the keyboard hook.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Variant::{VARIANT, VT_I4, VariantClear};
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromEvent, HWINEVENTHOOK, IAccessible, SetWinEventHook,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_OBJECT_FOCUS, GetMessageW, MSG, STATE_SYSTEM_PROTECTED,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

static PASSWORD_FIELD: AtomicBool = AtomicBool::new(false);

/// True while the focused control is a password (protected) field.
pub fn in_password_field() -> bool {
    PASSWORD_FIELD.load(Ordering::Relaxed)
}

pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("ezhuthu-focus".into())
        .spawn(|| {
            // SAFETY: COM is initialised for this thread only; the WinEvent
            // hook delivers callbacks to this thread's message loop below.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let hook = SetWinEventHook(
                    EVENT_OBJECT_FOCUS,
                    EVENT_OBJECT_FOCUS,
                    None,
                    Some(focus_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                if hook.0.is_null() {
                    eprintln!("focus tracking unavailable (password fields not detected)");
                    return;
                }
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("focus thread failed to start: {e}");
    }
}

unsafe extern "system" fn focus_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    PASSWORD_FIELD.store(is_protected(hwnd, id_object, id_child), Ordering::Relaxed);
}

fn is_protected(hwnd: HWND, id_object: i32, id_child: i32) -> bool {
    // SAFETY: standard MSAA calls; both VARIANTs are cleared after use, and the
    // union field is only read when `vt` says it holds an i32.
    unsafe {
        let mut acc: Option<IAccessible> = None;
        let mut child = VARIANT::default();
        if AccessibleObjectFromEvent(
            hwnd,
            id_object as u32,
            id_child as u32,
            &mut acc,
            &mut child,
        )
        .is_err()
        {
            return false;
        }
        let state = acc.map(|a| a.get_accState(&child));
        let _ = VariantClear(&mut child);
        let Some(Ok(mut state)) = state else {
            return false;
        };
        let inner = &state.Anonymous.Anonymous;
        let bits = if inner.vt == VT_I4 {
            inner.Anonymous.lVal as u32
        } else {
            0
        };
        let _ = VariantClear(&mut state);
        bits & STATE_SYSTEM_PROTECTED != 0
    }
}
