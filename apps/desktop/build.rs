use std::path::Path;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");
    embed_icon(Path::new("assets/neria.ico"));
}

/// Put `ico` into the exe as its icon (Explorer, taskbar, Alt-Tab). There's no rc.exe here,
/// so this writes the compiled resource (.res) itself; the MSVC linker takes it as an input.
fn embed_icon(ico: &Path) {
    println!("cargo:rerun-if-changed={}", ico.display());
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows_msvc {
        return;
    }
    let data = std::fs::read(ico).expect("reading the app icon");
    let res = icon_res(&data).expect("assets/neria.ico isn't a valid .ico file");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("neria-icon.res");
    std::fs::write(&out, res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

/// A .res file holding every image of the .ico (RT_ICON 1..n) and the group that lists
/// them (RT_GROUP_ICON 1, which Windows picks as the exe's icon).
fn icon_res(ico: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(ico.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(ico.get(o..o + 4)?.try_into().ok()?));
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = u16_at(4)? as usize;
    let mut res = Vec::new();
    resource(&mut res, 0, 0, 0, &[]); // the empty entry every .res starts with
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let (size, offset) = (u32_at(e + 8)? as usize, u32_at(e + 12)? as usize);
        let image = ico.get(offset..offset + size)?;
        let id = i as u16 + 1;
        resource(&mut res, RT_ICON, id, 0x1010, image);
        // GRPICONDIRENTRY: the ICONDIRENTRY with the file offset replaced by the resource id.
        group.extend_from_slice(ico.get(e..e + 12)?);
        group.extend_from_slice(&id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    Some(res)
}

fn resource(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes()); // header size
    for v in [0xFFFF, kind, 0xFFFF, id] {
        out.extend_from_slice(&u16::to_le_bytes(v)); // type and name as ordinals
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&(if kind == 0 { 0 } else { LANG_EN_US }).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
