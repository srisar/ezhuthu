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
    fn rejects_bad_hotkeys() {
        assert!(Hotkey::parse("T").is_err(), "no modifier");
        assert!(Hotkey::parse("Ctrl+Alt").is_err(), "no key");
        assert!(Hotkey::parse("Ctrl+T+Y").is_err(), "two keys");
        assert!(Hotkey::parse("Ctrl+F25").is_err());
        assert!(Hotkey::parse("Ctrl+Hyper").is_err());
    }
}
