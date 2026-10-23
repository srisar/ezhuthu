// Turns .ico files into a Win32 .res file of icon resources. The MSVC linker
// takes a .res as an input like an object file, so the exes get their icons
// without rc.exe or an extra crate. Shared by the app's and the setup's
// build scripts (`#[path]`); the icons themselves come from tools/icons.

use std::path::Path;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
/// MOVEABLE | PURE | DISCARDABLE, as rc.exe writes for icons (ignored by Win32).
const FLAGS: u16 = 0x1030;

/// One icon group per (resource id, .ico file); the lowest id is the one
/// Explorer shows for the exe.
pub fn icons(groups: &[(u16, impl AsRef<Path>)]) -> Vec<u8> {
    let mut res = Vec::new();
    // Every .res starts with an empty entry.
    entry(&mut res, 0, 0, 0, &[]);
    let mut next_image = 1u16;
    for (id, path) in groups {
        let path = path.as_ref();
        let ico = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        // GRPICONDIR: the ICONDIR header, then each ICONDIRENTRY with the
        // image offset replaced by the RT_ICON id holding that image.
        let mut dir = ico[..6].to_vec();
        for e in ico[6..6 + 16 * count].as_chunks::<16>().0 {
            let len = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
            entry(
                &mut res,
                RT_ICON,
                next_image,
                FLAGS,
                &ico[offset..offset + len],
            );
            dir.extend_from_slice(&e[..12]);
            dir.extend_from_slice(&next_image.to_le_bytes());
            next_image += 1;
        }
        entry(&mut res, RT_GROUP_ICON, *id, FLAGS, &dir);
    }
    res
}

/// A RESOURCEHEADER with numeric type and name, language neutral, then the
/// data padded to 4 bytes.
fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    for v in [0xFFFF, kind, 0xFFFF, name] {
        res.extend_from_slice(&u16::to_le_bytes(v));
    }
    res.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&0u16.to_le_bytes()); // LanguageId
    res.extend_from_slice(&0u32.to_le_bytes()); // Version
    res.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(data);
    res.resize(res.len().next_multiple_of(4), 0);
}
