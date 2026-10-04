//! 按行流式读取大 jsonl 文件：先看行首决定要不要，不要的行（比如带 base64 图片的超长行）直接跳过，
//! 不整份读进内存。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// 交给 `LineSink::want` 的行首长度
pub(crate) const HEAD: usize = 512;
const BUF_SIZE: usize = 1 << 20;

pub(crate) trait LineSink {
    /// 只根据行首（最多 `HEAD` 字节）判断这一行要不要完整读出来。
    fn want(&mut self, head: &[u8]) -> bool;
    /// 处理一整行（不含换行符）。返回这一行是否被成功解析；
    /// 只用于判断文件末尾没有换行的那一行算不算读完。
    fn line(&mut self, line: &[u8]) -> bool;
}

fn head(line: &[u8]) -> &[u8] {
    &line[..line.len().min(HEAD)]
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn read_some(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match file.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 从 `offset` 开始逐行读取，返回已经完整处理过的字节位置（最后一个换行之后；
/// 若末尾没有换行的一行被成功解析，则是文件末尾）。
pub(crate) fn scan_lines(path: &Path, offset: u64, sink: &mut impl LineSink) -> io::Result<u64> {
    let mut file = File::open(path)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))?;
    }
    let mut buf = vec![0u8; BUF_SIZE];
    let (mut start, mut end) = (0usize, 0usize);
    // buf[start] 对应的文件位置
    let mut pos = offset;
    let mut committed = offset;
    // 正在拼接的超长目标行
    let mut long: Option<Vec<u8>> = None;
    // 正在跳过的超长无关行
    let mut skipping = false;

    loop {
        if let Some(i) = memchr::memchr(b'\n', &buf[start..end]) {
            let line = &buf[start..start + i];
            if skipping {
                skipping = false;
            } else if let Some(mut acc) = long.take() {
                acc.extend_from_slice(line);
                sink.line(trim_cr(&acc));
            } else if !line.is_empty() && sink.want(head(line)) {
                sink.line(trim_cr(line));
            }
            start += i + 1;
            pos += (i + 1) as u64;
            committed = pos;
            continue;
        }

        // 缓冲区里没有完整的一行
        let rem = end - start;
        if skipping {
            pos += rem as u64;
            start = end;
        } else if let Some(acc) = long.as_mut() {
            acc.extend_from_slice(&buf[start..end]);
            pos += rem as u64;
            start = end;
        } else if rem >= HEAD {
            if sink.want(&buf[start..start + HEAD]) {
                long = Some(buf[start..end].to_vec());
            } else {
                skipping = true;
            }
            pos += rem as u64;
            start = end;
        }
        if start > 0 {
            buf.copy_within(start..end, 0);
            end -= start;
            start = 0;
        }

        let n = read_some(&mut file, &mut buf[end..])?;
        if n == 0 {
            // 文件末尾没有换行的一行：可能是正在写入的半行，解析成功才算读完
            if !skipping {
                if let Some(acc) = long.take() {
                    if sink.line(trim_cr(&acc)) {
                        committed = pos;
                    }
                } else if end > start {
                    let tail = &buf[start..end];
                    if sink.want(head(tail)) && sink.line(trim_cr(tail)) {
                        committed = pos + (end - start) as u64;
                    }
                }
            }
            break;
        }
        end += n;
    }
    Ok(committed)
}

/// 文件开头 `len` 字节的指纹。
pub(crate) fn head_hash(path: &Path, len: u64) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)?;
    Ok(crate::record::fnv64(&[&buf]))
}

#[inline]
pub(crate) fn contains(hay: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(hay, needle).is_some()
}

/// 在原始 JSON 行里找 `"key":"value"` 并取出 value（不处理转义，只用于 id 这类简单值）。
pub(crate) fn find_str_value<'a>(line: &'a [u8], key_pat: &[u8]) -> Option<&'a str> {
    let at = memchr::memmem::find(line, key_pat)? + key_pat.len();
    let rest = &line[at..];
    let end = memchr::memchr(b'"', rest)?;
    std::str::from_utf8(&rest[..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Collect {
        lines: Vec<String>,
    }

    impl LineSink for Collect {
        fn want(&mut self, head: &[u8]) -> bool {
            head.starts_with(b"{\"want\"")
        }
        fn line(&mut self, line: &[u8]) -> bool {
            let ok = serde_json::from_slice::<serde_json::Value>(line).is_ok();
            if ok {
                self.lines.push(String::from_utf8_lossy(line).into_owned());
            }
            ok
        }
    }

    #[test]
    fn skips_long_lines_and_keeps_partial_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        let mut f = File::create(&p).unwrap();
        let big = "x".repeat(3 * BUF_SIZE);
        writeln!(f, "{{\"want\":1}}").unwrap();
        writeln!(f, "{{\"skip\":\"{big}\"}}").unwrap();
        writeln!(f, "{{\"want\":\"{big}\"}}").unwrap();
        writeln!(f, "{{\"want\":3}}").unwrap();
        write!(f, "{{\"want\":4").unwrap(); // 半行
        drop(f);
        let mut c = Collect { lines: vec![] };
        let off = scan_lines(&p, 0, &mut c).unwrap();
        assert_eq!(c.lines.len(), 3);
        assert_eq!(c.lines[0], "{\"want\":1}");
        assert!(c.lines[1].len() > 3 * BUF_SIZE);
        assert_eq!(c.lines[2], "{\"want\":3}");
        let len = std::fs::metadata(&p).unwrap().len();
        assert_eq!(off, len - "{\"want\":4".len() as u64);

        // 续写完半行后从 offset 继续
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "}}").unwrap();
        write!(f, "{{\"want\":5}}").unwrap(); // 完整但没有换行
        drop(f);
        let mut c = Collect { lines: vec![] };
        let off2 = scan_lines(&p, off, &mut c).unwrap();
        assert_eq!(c.lines, vec!["{\"want\":4}", "{\"want\":5}"]);
        assert_eq!(off2, std::fs::metadata(&p).unwrap().len());
    }
}
