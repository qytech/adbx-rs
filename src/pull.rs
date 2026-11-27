use anyhow::{bail, ensure, Context, Result};
use std::ffi::OsStr;
use std::fmt::Write;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Positional operands of adb push/pull, excluding option values and the optional wait prefix.
pub fn transfer_operands(args: &[String]) -> Result<Vec<usize>> {
    let mut operands = Vec::new();
    let mut literal = false;
    let prefix_len = args.len() - crate::adb::after_wait_prefix(args).len();
    let mut argv = args.iter().enumerate().skip(prefix_len + 1);
    while let Some((index, arg)) = argv.next() {
        if literal || !arg.starts_with('-') {
            operands.push(index);
            continue;
        }
        match arg.as_str() {
            "--" => literal = true,
            "-z" => {
                argv.next().context("pull -z 需要压缩算法")?;
            }
            "-n" | "-a" | "-p" | "-q" | "-Z" | "--sync" => {}
            other => bail!("无法解析传输参数 {other}"),
        }
    }
    Ok(operands)
}

/// Prepare every destination before starting any transfer. Keep adb's options and
/// remote operands verbatim, replacing only the local destination.
pub fn per_device_args(
    args: &[String],
    serials: &[String],
    cwd: Option<&Path>,
) -> Result<Vec<Vec<String>>> {
    let operands = transfer_operands(args)?;
    ensure!(!operands.is_empty(), "pull 需要至少一个远端路径");
    let dest_index = (operands.len() > 1).then(|| operands[operands.len() - 1]);
    let local = dest_index.map_or(".", |i| args[i].as_str());
    ensure!(!local.is_empty(), "多机 pull 需要有效的本地目录");
    let rooted = cwd.map(|dir| dir.join(local));
    let root = rooted.as_deref().unwrap_or_else(|| Path::new(local));
    match fs::metadata(root) {
        Ok(meta) => ensure!(
            meta.is_dir(),
            "多机 pull 的目标必须是目录：{}",
            root.display()
        ),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            ensure!(
                local.ends_with(std::path::is_separator) || operands.len() > 2,
                "多机 pull 无法将多台设备保存到同一个文件。要新建目录，请写作 {local}/"
            );
        }
        Err(e) => return Err(e).with_context(|| format!("无法访问 {}", root.display())),
    }

    fs::create_dir_all(root).with_context(|| format!("无法创建输出目录 {}", root.display()))?;
    let mut commands = Vec::with_capacity(serials.len());
    for serial in serials {
        let path = allocate_device_dir(root, serial)?;
        let mut command = args.to_vec();
        let dest = path
            .to_str()
            .context("设备输出目录不是 UTF-8 路径")?
            .to_owned();
        match dest_index {
            Some(i) => command[i] = dest,
            None => command.push(dest),
        }
        commands.push(command);
    }
    Ok(commands)
}

// The spelling stored by the filesystem identifies the owner of existing
// directories, including those created before case-collision handling existed.
fn allocate_device_dir(root: &Path, serial: &str) -> Result<PathBuf> {
    let name = directory_name(serial)?;
    let fallback_name = format!(
        "%s-{}",
        serial
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>()
    );
    let fallback = root.join(&fallback_name);
    let mut folded_collision = false;
    let mut exact = false;
    for child in fs::read_dir(root)? {
        let child = child?;
        let spelling = child.file_name();
        if spelling == OsStr::new(&fallback_name) {
            return existing_device_dir(&fallback);
        }
        exact |= spelling == OsStr::new(&name);
        if spelling != OsStr::new(&name)
            && spelling
                .to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(&name))
        {
            folded_collision = true;
        }
    }
    if exact {
        return existing_device_dir(&root.join(&name));
    }
    if !folded_collision {
        let path = root.join(&name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // Another transfer may have created a differently cased directory
                // between the scan and mkdir on a case-insensitive volume.
                for child in fs::read_dir(root)? {
                    if child?.file_name() == OsStr::new(&name) {
                        return existing_device_dir(&path);
                    }
                }
            }
            Err(e) => return Err(e).with_context(|| format!("无法创建 {}", path.display())),
        }
    }
    match fs::create_dir(&fallback) {
        Ok(()) => Ok(fallback),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => existing_device_dir(&fallback),
        Err(e) => Err(e).with_context(|| format!("无法创建 {}", fallback.display())),
    }
}

fn existing_device_dir(path: &Path) -> Result<PathBuf> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.is_symlink(),
        "设备输出目录被占用：{}",
        path.display()
    );
    Ok(path.to_owned())
}

// Preserve ordinary serials and encode bytes that cannot be used in a portable
// directory name. Escaping '%' itself keeps Wi-Fi serials distinct from USB names.
fn directory_name(serial: &str) -> Result<String> {
    ensure!(!serial.is_empty(), "设备序列号为空");
    let mut name = String::new();
    for (i, byte) in serial.bytes().enumerate() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_')
            || (byte == b'.' && i + 1 < serial.len())
        {
            name.push(char::from(byte));
        } else {
            write!(name, "%{byte:02X}").unwrap();
        }
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let numbered_port = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit());
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered_port {
        name.insert_str(0, "%00");
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_variants_in_different_batches_never_reuse_another_devices_output() {
        let out = tempfile::tempdir().unwrap();
        let args = vec![
            "pull".to_owned(),
            "/sdcard/run.log".to_owned(),
            out.path().to_str().unwrap().to_owned(),
        ];
        let batch = |first: &str| {
            let commands = per_device_args(&args, &[first.to_owned(), "B".into()], None).unwrap();
            PathBuf::from(commands[0].last().unwrap()).join("run.log")
        };
        let lower = batch("abc");
        fs::write(&lower, "first device").unwrap();
        let upper = batch("ABC");
        fs::write(&upper, "second device").unwrap();
        assert_eq!(fs::read_to_string(&lower).unwrap(), "first device");
        assert_eq!(fs::read_to_string(&upper).unwrap(), "second device");
        assert_eq!(batch("abc"), lower);
        assert_eq!(batch("ABC"), upper);
    }

    #[test]
    fn serials_form_distinct_portable_path_components() {
        for (serial, expected) in [
            ("ABC-123", "ABC-123"),
            ("192.168.1.8:5555", "192.168.1.8%3A5555"),
            ("A/B", "A%2FB"),
            ("A%2FB", "A%252FB"),
            ("..", ".%2E"),
            ("CON", "%00CON"),
            ("COM1.txt", "%00COM1.txt"),
        ] {
            assert_eq!(directory_name(serial).unwrap(), expected);
        }
    }
}
