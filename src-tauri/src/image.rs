//! Read image bytes (from the working tree or the staged index blob) and return
//! them base64-encoded with a MIME type, for inline preview in the diff view.
//!
//! Kept separate from the command layer so the pure parts (extension → MIME,
//! size-cap enforcement, workdir/blob round-trip) are unit-testable without a
//! Tauri `State`.

use std::path::Path;

use base64::Engine as _;
use git2::Repository;
use serde::Serialize;

use crate::error::{AppError, AppResult};

/// Hard ceiling on the decoded image size we will inline. Beyond this the
/// base64 payload (and the resulting data URL) is too large to be worth pushing
/// through the IPC bridge, so we fail with a clear message instead.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// An image ready to be rendered as a `data:` URL by the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageData {
    /// e.g. `image/png` — derived solely from the file extension.
    pub mime_type: String,
    /// Standard (padded) base64 of the raw image bytes.
    pub base64: String,
}

/// Map a file extension to an allow-listed image MIME type.
///
/// The allow-list is the security boundary: only these extensions are ever read
/// and returned, so a mis-typed or hostile path pointing at a non-image file is
/// rejected before any bytes are read.
pub fn mime_from_extension(path: &str) -> AppResult<&'static str> {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let ext = ext
        .as_deref()
        .ok_or_else(|| AppError::validation("File has no extension; not a supported image"))?;

    let mime = match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        other => {
            return Err(AppError::validation(format!(
                "Unsupported image type: .{other}"
            )))
        }
    };
    Ok(mime)
}

/// Read `path` (working-tree file when `staged == false`, else the staged index
/// blob) and return it base64-encoded with its MIME type.
///
/// `max_bytes` is a parameter (not the constant) so tests can exercise the cap
/// without a 20 MB fixture; the command layer passes [`MAX_IMAGE_BYTES`].
pub fn read_image_data(
    repo: &Repository,
    path: &str,
    staged: bool,
    max_bytes: usize,
) -> AppResult<ImageData> {
    let mime = mime_from_extension(path)?;

    let bytes = if staged {
        read_staged_blob(repo, path, max_bytes)?
    } else {
        read_workdir_file(repo, path, max_bytes)?
    };

    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ImageData {
        mime_type: mime.to_string(),
        base64,
    })
}

/// Read the working-tree file at `path`, enforcing the size cap from `metadata`
/// first so an oversized file is never loaded into memory.
fn read_workdir_file(repo: &Repository, path: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| AppError::validation("Repository has no working tree"))?;
    let full = workdir.join(path);

    // Reject symlinks *before* reading (`symlink_metadata` does not follow the
    // link): a symlink inside the working tree could point outside the repo, so
    // following it would let an image-preview request read an arbitrary file. The
    // path string itself is already lexically validated upstream; this closes the
    // in-tree-symlink escape.
    let meta = std::fs::symlink_metadata(&full)?;
    if meta.file_type().is_symlink() {
        return Err(AppError::validation(format!(
            "Refusing to read '{path}': it is a symbolic link"
        )));
    }
    // Compare in `u64` so no cast is needed on the hot path; only build the
    // `usize` actual-size for the error message, saturating on 32-bit targets
    // (macOS is 64-bit, where this is exact and lossless).
    if meta.len() > max_bytes as u64 {
        let actual = usize::try_from(meta.len()).unwrap_or(usize::MAX);
        return Err(too_large(actual, max_bytes));
    }
    Ok(std::fs::read(&full)?)
}

/// Read the blob recorded in the index (stage 0) for `path`.
fn read_staged_blob(repo: &Repository, path: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
    let index = repo.index()?;
    let entry = index.get_path(Path::new(path), 0).ok_or_else(|| {
        AppError::validation(format!("No staged version of '{path}' in the index"))
    })?;
    let blob = repo.find_blob(entry.id)?;
    let content = blob.content();
    if content.len() > max_bytes {
        return Err(too_large(content.len(), max_bytes));
    }
    Ok(content.to_vec())
}

fn too_large(actual: usize, max_bytes: usize) -> AppError {
    AppError::validation(format!(
        "Image is too large ({actual} bytes); the maximum is {max_bytes} bytes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::setup;
    use std::fs;
    use std::path::Path as StdPath;

    /// The smallest possible valid PNG: an 8-byte signature followed by a
    /// zero-length IHDR-less stream is *not* a real PNG, so we embed a genuine
    /// 1x1 transparent PNG (67 bytes) instead. Bytes verified with `file`.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn mime_from_extension_allow_list() {
        assert_eq!(mime_from_extension("a/b.png").unwrap(), "image/png");
        assert_eq!(mime_from_extension("x.JPG").unwrap(), "image/jpeg");
        assert_eq!(mime_from_extension("x.jpeg").unwrap(), "image/jpeg");
        assert_eq!(mime_from_extension("x.gif").unwrap(), "image/gif");
        assert_eq!(mime_from_extension("x.webp").unwrap(), "image/webp");
        assert_eq!(mime_from_extension("x.bmp").unwrap(), "image/bmp");
        assert_eq!(mime_from_extension("x.ico").unwrap(), "image/x-icon");
        assert_eq!(mime_from_extension("x.avif").unwrap(), "image/avif");
    }

    #[test]
    fn mime_rejects_non_image_and_extensionless() {
        assert!(mime_from_extension("notes.txt").is_err());
        assert!(mime_from_extension("archive.zip").is_err());
        assert!(mime_from_extension("Makefile").is_err());
    }

    #[test]
    fn reads_workdir_png_and_encodes_base64() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("pic.png"), TINY_PNG).unwrap();

        let data = read_image_data(&repo, "pic.png", false, MAX_IMAGE_BYTES).unwrap();
        assert_eq!(data.mime_type, "image/png");

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data.base64.as_bytes())
            .unwrap();
        assert_eq!(decoded, TINY_PNG);
    }

    #[test]
    fn reads_staged_blob_round_trip() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("pic.png"), TINY_PNG).unwrap();

        // Stage it so the index carries the blob, then overwrite the working
        // copy to prove the staged read comes from the index, not the disk file.
        let mut index = repo.index().unwrap();
        index.add_path(StdPath::new("pic.png")).unwrap();
        index.write().unwrap();
        fs::write(dir.path().join("pic.png"), b"clobbered-not-an-image").unwrap();

        let staged = read_image_data(&repo, "pic.png", true, MAX_IMAGE_BYTES).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(staged.base64.as_bytes())
            .unwrap();
        assert_eq!(
            decoded, TINY_PNG,
            "staged read must come from the index blob"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_workdir_symlink() {
        // A symlink in the working tree (even one that resolves to a real image)
        // must be refused rather than followed — it is the in-tree path-escape
        // vector.
        let (dir, repo) = setup();
        let real = dir.path().join("real.png");
        fs::write(&real, TINY_PNG).unwrap();
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = read_image_data(&repo, "link.png", false, MAX_IMAGE_BYTES)
            .unwrap_err()
            .to_string();
        assert!(err.contains("symbolic link"), "{err}");
    }

    #[test]
    fn rejects_extension_not_in_allow_list() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("data.txt"), b"hello").unwrap();
        assert!(read_image_data(&repo, "data.txt", false, MAX_IMAGE_BYTES).is_err());
    }

    #[test]
    fn enforces_size_cap_via_lowered_threshold() {
        // Faking the 20 MB case with a tiny threshold keeps the test fast; the
        // cap logic is identical regardless of the numeric limit.
        let (dir, repo) = setup();
        fs::write(dir.path().join("big.png"), TINY_PNG).unwrap();

        let err = read_image_data(&repo, "big.png", false, 8)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too large"), "{err}");

        // Staged path enforces the same cap.
        let mut index = repo.index().unwrap();
        index.add_path(StdPath::new("big.png")).unwrap();
        index.write().unwrap();
        let err = read_image_data(&repo, "big.png", true, 8)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too large"), "{err}");
    }
}
