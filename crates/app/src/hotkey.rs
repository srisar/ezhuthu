//! Parses hotkey strings such as "Ctrl+Alt+T" into RegisterHotKey arguments.

use anyhow::{Result, bail};

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u32,
}

impl Hotkey {
    pub fn parse(s: &str) -> Result<Self> {
        let mut modifiers = 0;
        let mut vk = None;
        for part in s.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= MOD_CONTROL,
                "alt" => modifiers |= MOD_ALT,
                "shift" => modifiers |= MOD_SHIFT,
                "win" | "windows" => modifiers |= MOD_WIN,
                key => {
                    if vk.is_some() {
                        bail!("hotkey {s:?} has more than one key");
                    }
                    vk = Some(
                        parse_key(key)
                            .ok_or_else(|| anyhow::anyhow!("unknown key {part:?} in {s:?}"))?,
                    );
                }
            }
        }
        let Some(vk) = vk else {
            bail!("hotkey {s:?} has no key")
        };
        if modifiers == 0 {
            bail!("hotkey {s:?} needs at least one of Ctrl, Alt, Shift, Win");
        }
        Ok(Self { modifiers, vk })
    }

    /// A combination pressed in the Settings window's key box: `modifiers`
    /// (MOD_* bits) held while `vk` went down. Needs Ctrl, Alt or Win (Shift
    /// alone would turn ordinary capital letters into the toggle) and a key
    /// we can store by name. Ctrl+Space and friends are fine.
    pub fn from_keys(modifiers: u32, vk: u32) -> Option<Self> {
        let strong = modifiers & (MOD_CONTROL | MOD_ALT | MOD_WIN) != 0;
        (strong && key_name(vk).is_some()).then_some(Self { modifiers, vk })
    }
}

/// "Ctrl+Alt+T": the form stored in the settings file and shown to users.
impl std::fmt::Display for Hotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (bit, name) in [
            (MOD_CONTROL, "Ctrl"),
            (MOD_ALT, "Alt"),
            (MOD_SHIFT, "Shift"),
            (MOD_WIN, "Win"),
        ] {
            if self.modifiers & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.vk).unwrap_or_else(|| format!("0x{:02X}", self.vk)))
    }
}

/// The name `parse_key` accepts for a virtual key.
fn key_name(vk: u32) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(vk as u8).to_string()),
        0x20 => Some("Space".into()),
        0x0D => Some("Enter".into()),
        0x09 => Some("Tab".into()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        _ => None,
    }
}

fn parse_key(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    match key {
        _ if b.len() == 1 && b[0].is_ascii_alphanumeric() => Some(b[0].to_ascii_uppercase() as u32),
        "space" => Some(0x20),
        "enter" | "return" => Some(0x0D),
        "tab" => Some(0x09),
        _ if key.starts_with('f') => {
            let n: u32 = key[1..].parse().ok()?;
            (1..=24).contains(&n).then_some(0x70 + n - 1) // VK_F1 = 0x70
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hotkeys() {
        assert_eq!(
            Hotkey::parse("Ctrl+Alt+T").unwrap(),
            Hotkey {
                modifiers: MOD_CONTROL | MOD_ALT,
                vk: b'T' as u32
            }
        );
        assert_eq!(
            Hotkey::parse("win + shift + space").unwrap(),
            Hotkey {
                modifiers: MOD_WIN | MOD_SHIFT,
                vk: 0x20
            }
        );
        assert_eq!(Hotkey::parse("Alt+F12").unwrap().vk, 0x7B);
    }

    #[test]
    fn round_trips_through_text() {
        for s in [
            "Ctrl+Alt+T",
            "Ctrl+Space",
            "Ctrl+Shift+F5",
            "Alt+Space",
            "Ctrl+1",
            "Win+Enter",
        ] {
            assert_eq!(Hotkey::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn captures_pressed_combinations() {
        let ctrl_space = Hotkey::from_keys(MOD_CONTROL, 0x20).unwrap();
        assert_eq!(ctrl_space.to_string(), "Ctrl+Space");
        assert_eq!(
            Hotkey::from_keys(MOD_ALT | MOD_SHIFT, 0x09)
                .unwrap()
                .to_string(),
            "Alt+Shift+Tab"
        );
        assert_eq!(Hotkey::from_keys(0, b'T' as u32), None, "no modifier");
        assert_eq!(
            Hotkey::from_keys(MOD_SHIFT, b'T' as u32),
            None,
            "Shift alone types capitals"
        );
        assert_eq!(
            Hotkey::from_keys(MOD_CONTROL, 0xBC),
            None,
            "comma has no stored name"
        );
    }

    #[test]
    fn rejects_bad_hotkeys() {
        assert!(Hotkey::parse("T").is_err(), "no modifier");
        assert!(Hotkey::parse("Ctrl+Alt").is_err(), "no key");
        assert!(Hotkey::parse("Ctrl+T+Y").is_err(), "two keys");
        assert!(Hotkey::parse("Ctrl+F25").is_err());
        assert!(Hotkey::parse("Ctrl+Hyper").is_err());
    }
}
