use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_ENVIRONMENT: &str = "/run/pocketforge/session/environment";

/// The compositor publishes this file atomically after its socket is ready.  The
/// digest is an opaque client-side identity for that publication; it is not a
/// second authority or a replacement for the publisher's lifecycle generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompositorSession {
    pub(crate) wayland_display: PathBuf,
    pub(crate) publication: String,
}

impl CompositorSession {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path)
            .map_err(|error| format!("read compositor environment {}: {error}", path.display()))?;
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            format!(
                "compositor environment {} is not UTF-8: {error}",
                path.display()
            )
        })?;
        let mut wayland_display = None;
        for raw_line in text.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!(
                    "compositor environment {} has malformed line",
                    path.display()
                )
            })?;
            if key.is_empty() || value.is_empty() {
                return Err(format!(
                    "compositor environment {} has an empty key or value",
                    path.display()
                ));
            }
            if key == "WAYLAND_DISPLAY" {
                if wayland_display.is_some() {
                    return Err(format!(
                        "compositor environment {} repeats WAYLAND_DISPLAY",
                        path.display()
                    ));
                }
                let display = PathBuf::from(value);
                if !display.is_absolute() {
                    return Err(format!(
                        "compositor environment {} requires an absolute WAYLAND_DISPLAY",
                        path.display()
                    ));
                }
                wayland_display = Some(display);
            }
        }
        let wayland_display = wayland_display.ok_or_else(|| {
            format!(
                "compositor environment {} does not publish WAYLAND_DISPLAY",
                path.display()
            )
        })?;
        let publication = format!("{:x}", Sha256::digest(&bytes));
        Ok(Self {
            wayland_display,
            publication,
        })
    }

    pub(crate) fn reload(path: &Path, previous: &Self) -> Result<Self, String> {
        let next = Self::load(path)?;
        if next.publication == previous.publication {
            return Err(format!(
                "stale compositor environment publication {}",
                path.display()
            ));
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn accepts_absolute_wayland_socket_and_tracks_publication() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("environment");
        fs::write(
            &path,
            "WAYLAND_DISPLAY=/run/pocketforge/session/wayland-0\n",
        )
        .expect("write");
        let session = CompositorSession::load(&path).expect("publication");
        assert_eq!(
            session.wayland_display,
            PathBuf::from("/run/pocketforge/session/wayland-0")
        );
        assert_eq!(session.publication.len(), 64);
    }

    #[test]
    fn rejects_missing_or_relative_socket_publication() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("environment");
        fs::write(&path, "DISPLAY=:0\n").expect("write");
        assert!(CompositorSession::load(&path).is_err());
        fs::write(&path, "WAYLAND_DISPLAY=wayland-0\n").expect("write");
        assert!(CompositorSession::load(&path).is_err());
    }

    #[test]
    fn rejects_stale_publication_on_reload() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("environment");
        fs::write(
            &path,
            "WAYLAND_DISPLAY=/run/pocketforge/session/wayland-0\n",
        )
        .expect("write");
        let session = CompositorSession::load(&path).expect("publication");
        assert!(CompositorSession::reload(&path, &session).is_err());
        fs::write(
            &path,
            "WAYLAND_DISPLAY=/run/pocketforge/session/wayland-1\n",
        )
        .expect("write");
        assert!(CompositorSession::reload(&path, &session).is_ok());
    }
}
