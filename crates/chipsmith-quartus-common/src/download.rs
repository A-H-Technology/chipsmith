use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use chipsmith_toolchain::error::ChipsmithError;

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .expect("could not determine home directory")
                .join(".cache")
        })
        .join("chipsmith")
}

pub async fn download_file(url: &str, filename: &str) -> Result<PathBuf, ChipsmithError> {
    let cache = cache_dir();
    fs::create_dir_all(&cache).await?;

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
        .map_err(|e| ChipsmithError::Download {
            message: e.to_string(),
        })?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| ChipsmithError::Download {
            message: e.to_string(),
        })?
        .error_for_status()
        .map_err(|e| ChipsmithError::Download {
            message: e.to_string(),
        })?;
    let total = response.content_length();
    let mut stream = response.bytes_stream();

    let tmp = cache.join(format!("{}.part", filename));
    let mut file = fs::File::create(&tmp).await?;
    let mut downloaded: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ChipsmithError::Download {
            message: e.to_string(),
        })?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if let Some(total) = total {
            eprint!("\r  {:.0}%", (downloaded as f64 / total as f64) * 100.0);
        }
    }
    eprintln!();

    file.flush().await?;
    drop(file);

    fs::rename(&tmp, &dest).await?;

    eprintln!("Saved to {}", dest.display());
    Ok(dest)
}

pub async fn make_executable(path: &Path) -> Result<(), ChipsmithError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).await?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).await?;
    Ok(())
}

pub async fn unzip(archive: &Path, dest: &Path) -> Result<(), ChipsmithError> {
    let status = tokio::process::Command::new("unzip")
        .arg("-q")
        .arg("-o")
        .arg(archive)
        .arg("-d")
        .arg(dest)
        .spawn()
        .map_err(|source| ChipsmithError::Spawn {
            command: "unzip".to_string(),
            source,
        })?
        .wait()
        .await?;

    if !status.success() {
        return Err(ChipsmithError::ProcessFailed {
            command: format!("unzip {}", archive.display()),
            code: status.code(),
        });
    }

    Ok(())
}
