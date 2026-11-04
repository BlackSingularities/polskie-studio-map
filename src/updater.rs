//! Aktualizacje przez redzikowski.com: serwer strony udostępnia kopię GitHub Releases w tym samym
//! formacie i pośredniczy w pobieraniu plików. Instalator jest sprawdzany sumą SHA-512 z `latest.yml`
//! tego samego wydania i uruchamiany z podniesieniem uprawnień (UAC) w trybie cichym.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use sha2::{Digest, Sha512};

use crate::download::agent;

const FEED: &str = "https://redzikowski.com/api/github/repos/BlackSingularities/polskie-studio-map/releases/latest";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub exe_name: String,
    pub exe_url: String,
    pub size: u64,
    yml_url: Option<String>,
    pub notes: String,
}

/// Porównanie wersji x.y.z: dodatnie, gdy a > b.
pub fn cmp_version(a: &str, b: &str) -> i64 {
    let p = |s: &str| -> Vec<i64> {
        s.trim_start_matches('v')
            .split(['.', '-'])
            .take(3)
            .map(|n| n.parse().unwrap_or(0))
            .collect()
    };
    let (pa, pb) = (p(a), p(b));
    for i in 0..3 {
        let (x, y) = (pa.get(i).copied().unwrap_or(0), pb.get(i).copied().unwrap_or(0));
        if x != y {
            return x - y;
        }
    }
    0
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn latest() -> Result<Release, String> {
    let body = agent()
        .get(FEED)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("redzikowski.com: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let version = v["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string();
    let empty = Vec::new();
    let assets = v["assets"].as_array().unwrap_or(&empty);
    let exe = assets
        .iter()
        .find(|a| {
            let n = a["name"].as_str().unwrap_or("");
            n.starts_with("PSM-Setup-") && n.to_lowercase().ends_with(".exe")
        })
        .ok_or("Wydanie nie zawiera instalatora.")?;
    let yml = assets.iter().find(|a| a["name"] == "latest.yml");
    Ok(Release {
        version,
        exe_name: exe["name"].as_str().unwrap_or("").to_string(),
        exe_url: exe["browser_download_url"].as_str().unwrap_or("").to_string(),
        size: exe["size"].as_u64().unwrap_or(0),
        yml_url: yml.and_then(|y| y["browser_download_url"].as_str().map(String::from)),
        notes: v["body"].as_str().unwrap_or("").to_string(),
    })
}

/// Suma SHA-512 (base64) instalatora z `latest.yml`.
fn expected_sha(rel: &Release) -> Result<String, String> {
    let url = rel.yml_url.as_ref().ok_or("Wydanie nie zawiera pliku latest.yml z sumą kontrolną.")?;
    let text = agent().get(url).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())?;
    parse_sha(&text, &rel.exe_name).ok_or_else(|| "Brak sumy SHA-512 w latest.yml.".into())
}

pub fn parse_sha(yml: &str, name: &str) -> Option<String> {
    // blok „- url: <nazwa>” w sekcji files, a w razie czego główne pole sha512
    let block = yml
        .split("- url:")
        .find(|b| b.trim_start().starts_with(name))
        .unwrap_or(yml);
    block
        .lines()
        .find_map(|l| l.trim().strip_prefix("sha512:"))
        .map(|s| s.trim().to_string())
}

/// Pobranie instalatora do katalogu tymczasowego (postęp w bajtach) i sprawdzenie sumy.
pub fn download(rel: &Release, progress: Arc<AtomicU64>) -> Result<PathBuf, String> {
    let sha = expected_sha(rel)?;
    let dir = std::env::temp_dir().join("PSM-aktualizacja");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(&rel.exe_name);
    let r = agent()
        .get(&rel.exe_url)
        .timeout(Duration::from_secs(1800))
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader = r.into_reader();
    let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    let mut hash = Sha512::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        progress.fetch_add(n as u64, Ordering::Relaxed);
    }
    drop(out);
    let got = base64::engine::general_purpose::STANDARD.encode(hash.finalize());
    if got != sha {
        let _ = std::fs::remove_file(&file);
        return Err("Pobrany instalator ma niezgodną sumę kontrolną – spróbuj ponownie później.".into());
    }
    Ok(file)
}

/// Instalator w trybie cichym z oknem UAC; po instalacji uruchamia nową wersję.
pub fn run_installer(file: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let w = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
        let verb = w("runas".as_ref());
        let path = w(file.as_os_str());
        let args = w("/S --updated --force-run".as_ref());
        // SAFETY: wszystkie napisy zakończone zerem i żyją do końca wywołania
        let r = unsafe {
            ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), path.as_ptr(), args.as_ptr(), std::ptr::null(), SW_SHOWNORMAL)
        };
        if (r as isize) <= 32 {
            return Err("Instalacja aktualizacji została przerwana (brak zgody administratora?).".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Err("Aktualizacje tylko w systemie Windows.".into())
    }
}

/// Pozostałość po poprzedniej aktualizacji.
pub fn cleanup() {
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("PSM-aktualizacja"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(cmp_version("2.0.0", "1.1.0") > 0);
        assert!(cmp_version("v1.0.3", "1.0.3") == 0);
        assert!(cmp_version("2.0.0", "2.0.1") < 0);
    }

    #[test]
    fn sha_from_yml() {
        let yml = "version: 2.0.0\nfiles:\n  - url: PSM-Setup-2.0.0.exe\n    sha512: abc+/=\n    size: 10\npath: PSM-Setup-2.0.0.exe\nsha512: xyz\n";
        assert_eq!(parse_sha(yml, "PSM-Setup-2.0.0.exe").as_deref(), Some("abc+/="));
    }
}
