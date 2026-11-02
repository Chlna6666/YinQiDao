//! Safe, bounded import of a single provider plugin ZIP.
//! ZIP is a distribution envelope, not an executable plug-in format.
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use crc32fast::Hasher;
use flate2::read::DeflateDecoder;

const MAX_ARCHIVE_BYTES: u64 = 192 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 4_096;
const MAX_PATH_BYTES: usize = 512;

pub(super) struct ExtractedPlugin {
    root: PathBuf,
    pub(super) package_dir: PathBuf,
}

impl Drop for ExtractedPlugin {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn u16le(s: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(s.get(offset..offset + 2).ok_or_else(|| anyhow!("ZIP header truncated"))?.try_into()?))
}

fn u32le(s: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(s.get(offset..offset + 4).ok_or_else(|| anyhow!("ZIP header truncated"))?.try_into()?))
}

fn checked_path(raw: &str) -> Result<PathBuf> {
    if raw.is_empty() || raw.len() > MAX_PATH_BYTES || raw.contains('\\') || raw.contains(':')
        || raw.bytes().any(|b| b == 0 || b < 0x20)
    {
        bail!("ZIP 包含非法路径");
    }
    let path = Path::new(raw);
    if path.is_absolute() || path.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("ZIP 包含目录穿越路径: {raw}");
    }
    Ok(path.to_owned())
}

pub(super) fn extract(path: &Path) -> Result<ExtractedPlugin> {
    let mut archive = File::open(path).with_context(|| format!("无法打开插件 ZIP: {}", path.display()))?;
    let size = archive.metadata()?.len();
    if !(22..=MAX_ARCHIVE_BYTES).contains(&size) {
        bail!("ZIP 压缩包大小不合法或超过限制");
    }
    // EOCD is at most 65,535 bytes before EOF. Multi-disk and ZIP64 are rejected.
    let search_len = size.min(65_557) as usize;
    archive.seek(SeekFrom::End(-(search_len as i64)))?;
    let mut tail = vec![0; search_len];
    archive.read_exact(&mut tail)?;
    let eocd = tail.windows(4).rposition(|s| s == b"PK\x05\x06")
        .ok_or_else(|| anyhow!("ZIP 缺少 End of Central Directory"))?;
    let end = tail.get(eocd..).ok_or_else(|| anyhow!("invalid EOCD"))?;
    if end.len() < 22 || u16le(end, 4)? != 0 || u16le(end, 6)? != 0
        || u16le(end, 8)? != u16le(end, 10)?
    {
        bail!("不支持多卷或损坏的 ZIP 压缩包");
    }
    let count = usize::from(u16le(end, 10)?);
    let cd_size = u64::from(u32le(end, 12)?);
    let cd_offset = u64::from(u32le(end, 16)?);
    let comment_len = usize::from(u16le(end, 20)?);
    if count == 0 || count > MAX_ENTRIES || cd_size == u64::from(u32::MAX)
        || cd_offset == u64::from(u32::MAX)
        || comment_len + 22 != end.len()
        || cd_offset.checked_add(cd_size).is_none_or(|end| end > size)
    {
        bail!("ZIP 目录索引无效、超出预算或需要 ZIP64");
    }

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root = std::env::temp_dir().join(format!("yinqidao-zip-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).context("创建 ZIP 插件导入暂存目录失败")?;
    let stage = ExtractedPlugin { root: root.clone(), package_dir: root.clone() };
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut cursor = cd_offset;
    for _ in 0..count {
        archive.seek(SeekFrom::Start(cursor))?;
        let mut central = [0u8; 46];
        archive.read_exact(&mut central)?;
        if &central[..4] != b"PK\x01\x02" {
            bail!("ZIP 中央目录签名不合法");
        }
        let flags = u16le(&central, 8)?;
        let method = u16le(&central, 10)?;
        let expected_crc = u32le(&central, 16)?;
        let compressed = u64::from(u32le(&central, 20)?);
        let uncompressed = u64::from(u32le(&central, 24)?);
        let name_len = usize::from(u16le(&central, 28)?);
        let extra_len = usize::from(u16le(&central, 30)?);
        let comment_len = usize::from(u16le(&central, 32)?);
        let disk = u16le(&central, 34)?;
        let mode = u32le(&central, 38)? >> 16;
        let local_offset = u64::from(u32le(&central, 42)?);
        cursor = cursor.checked_add(46 + (name_len + extra_len + comment_len) as u64)
            .ok_or_else(|| anyhow!("ZIP 目录偏移溢出"))?;
        if cursor > cd_offset + cd_size || name_len == 0 || name_len > MAX_PATH_BYTES
            || disk != 0 || flags & 1 != 0 || !matches!(method, 0 | 8)
            || compressed > MAX_ARCHIVE_BYTES || uncompressed > MAX_FILE_BYTES
            || local_offset == u64::from(u32::MAX)
        {
            bail!("ZIP 文件存在不支持的加密、压缩方法、ZIP64 或资源超额");
        }
        let kind = mode & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            bail!("插件 ZIP 不允许符号链接或特殊文件");
        }
        archive.seek(SeekFrom::Start(cursor - (name_len + extra_len + comment_len) as u64))?;
        let mut name_bytes = vec![0u8; name_len];
        archive.read_exact(&mut name_bytes)?;
        let name = std::str::from_utf8(&name_bytes).context("插件 ZIP 文件名必须是 UTF-8")?;
        let is_dir = name.ends_with('/');
        let raw_path = name.trim_end_matches('/');
        let relative = checked_path(raw_path)?;
        if !seen.insert(raw_path.to_ascii_lowercase()) {
            bail!("ZIP 包含重复或大小写冲突文件: {raw_path}");
        }
        let target = root.join(relative);
        if is_dir {
            if kind == 0o100000 {
                bail!("ZIP 文件类型与目录声明冲突");
            }
            fs::create_dir_all(&target)?;
            continue;
        }
        if kind == 0o040000 {
            bail!("ZIP 文件类型与普通文件声明冲突");
        }
        total = total.checked_add(uncompressed).ok_or_else(|| anyhow!("ZIP 解压大小溢出"))?;
        if total > MAX_TOTAL_BYTES {
            bail!("ZIP 解压总量超出限制");
        }
        archive.seek(SeekFrom::Start(local_offset))?;
        let mut local = [0u8; 30];
        archive.read_exact(&mut local)?;
        if &local[..4] != b"PK\x03\x04" || u16le(&local, 8)? != method
            || u16le(&local, 6)? != flags
            || usize::from(u16le(&local, 26)?) != name_len
        {
            bail!("ZIP 本地文件头与目录索引不一致");
        }
        let local_extra = u64::from(u16le(&local, 28)?);
        let data_offset = local_offset + 30 + name_len as u64 + local_extra;
        if data_offset.checked_add(compressed).is_none_or(|end| end > size) {
            bail!("ZIP 数据块超出压缩文件边界");
        }
        let mut local_name = vec![0u8; name_len];
        archive.read_exact(&mut local_name)?;
        if local_name != name_bytes {
            bail!("ZIP 本地文件名与中央目录不一致");
        }
        archive.seek(SeekFrom::Start(data_offset))?;
        let compressed_reader = (&mut archive).take(compressed);
        let mut reader: Box<dyn Read + '_> = if method == 8 {
            Box::new(DeflateDecoder::new(compressed_reader))
        } else {
            Box::new(compressed_reader)
        };
        let mut payload = Vec::with_capacity((uncompressed as usize).min(1024 * 1024));
        reader.by_ref().take(uncompressed + 1).read_to_end(&mut payload)?;
        if payload.len() as u64 != uncompressed {
            bail!("ZIP 解压大小不匹配或超过索引声明");
        }
        let mut crc = Hasher::new();
        crc.update(&payload);
        if crc.finalize() != expected_crc {
            bail!("ZIP 插件文件校验失败: {raw_path}");
        }
        if let Some(parent) = target.parent() { fs::create_dir_all(parent)?; }
        fs::write(&target, payload)?;
    }
    let package_dir = if root.join("plugin.toml").is_file() {
        root.clone()
    } else {
        let mut dirs = fs::read_dir(&root)?.collect::<std::io::Result<Vec<_>>>()?;
        if dirs.len() != 1 || !dirs.pop().is_some_and(|entry| entry.path().join("plugin.toml").is_file()) {
            bail!("插件 ZIP 必须包含 plugin.toml 与 provider.wasm，可使用一层顶级目录");
        }
        fs::read_dir(&root)?.next().ok_or_else(|| anyhow!("ZIP 空目录"))??.path()
    };
    if !package_dir.join("plugin.toml").is_file() {
        bail!("插件 ZIP 缺少 plugin.toml");
    }
    let mut extracted = stage;
    extracted.package_dir = package_dir;
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_parent_and_windows_absolute_paths() {
        for path in ["../escape", "/etc/passwd", "folder/../escape", "C:/Windows", "a\\..\\escape"] {
            assert!(checked_path(path).is_err(), "{path}");
        }
        assert!(checked_path("plugin.toml").is_ok());
    }
}
