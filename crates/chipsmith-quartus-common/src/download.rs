//! Fetching installers and Device Support packages, with a cache.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use chipsmith_toolchain::error::ChipsmithError;

use crate::process::{Capture, ProcessHost, ProcessSpec};

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .expect("could not determine home directory")
                .join(".cache")
        })
        .join("chipsmith")
}

/// Carry the URL and any HTTP status into the error, so a retired CDN path
/// is distinguishable from a transport failure without reading the message.
fn download_error(url: &str) -> impl Fn(reqwest::Error) -> ChipsmithError + '_ {
    move |e| ChipsmithError::Download {
        url: url.to_string(),
        status: e.status().map(|s| s.as_u16()),
        message: e.to_string(),
    }
}

pub async fn download_file(url: &str, filename: &str) -> Result<PathBuf, ChipsmithError> {
    let cache = cache_dir();
    fs::create_dir_all(&cache)
        .await
        .map_err(ChipsmithError::file("create cache directory", &cache))?;

    let dest = cache.join(filename);
    if dest.exists() {
        eprintln!("Using cached: {}", dest.display());
        return Ok(dest);
    }

    eprintln!("Downloading {} ...", url);

    // Altera's CDN sits behind Akamai, which rejects requests with no User-Agent.
    let client = Client::builder()
        .user_agent(concat!("chipsmith/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(download_error(url))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(download_error(url))?
        .error_for_status()
        .map_err(download_error(url))?;
    let total = response.content_length();
    let mut stream = response.bytes_stream();

    let tmp = cache.join(format!("{}.part", filename));
    let mut file = fs::File::create(&tmp)
        .await
        .map_err(ChipsmithError::file("create", &tmp))?;
    let mut downloaded: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(download_error(url))?;
        file.write_all(&chunk)
            .await
            .map_err(ChipsmithError::file("write", &tmp))?;
        downloaded += chunk.len() as u64;
        if let Some(total) = total {
            eprint!("\r  {:.0}%", (downloaded as f64 / total as f64) * 100.0);
        }
    }
    eprintln!();

    file.flush()
        .await
        .map_err(ChipsmithError::file("write", &tmp))?;
    drop(file);

    fs::rename(&tmp, &dest)
        .await
        .map_err(ChipsmithError::file("rename", &tmp))?;

    eprintln!("Saved to {}", dest.display());
    Ok(dest)
}

pub async fn make_executable(path: &Path) -> Result<(), ChipsmithError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .await
        .map_err(ChipsmithError::file("stat", path))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .await
        .map_err(ChipsmithError::file("make executable", path))?;
    Ok(())
}

pub async fn unzip(
    host: &dyn ProcessHost,
    archive: &Path,
    dest: &Path,
) -> Result<(), ChipsmithError> {
    let outcome = host
        .run(
            ProcessSpec::new("unzip")
                .args(["-q", "-o"])
                .arg(archive)
                .arg("-d")
                .arg(dest)
                .capture(Capture::Piped),
        )
        .await?;

    if !outcome.success() {
        return Err(ChipsmithError::ProcessFailed {
            command: format!("unzip {}", archive.display()),
            code: outcome.code,
            stderr_tail: outcome.stderr_tail,
        });
    }

    Ok(())
}
