//! `ImageLocator` over the library layer (DECISIONS B29): hash → current
//! adjacent sidecar location, composed entirely from public `Library` API.

use std::path::PathBuf;
use std::sync::Arc;

use photoproof_core::ContentHash;
use photoproof_core::library::Library;
use photoproof_core::sidecar::{AdjacentLocation, ImageLocator};

pub struct LibLocator(pub Arc<Library>);

impl ImageLocator for LibLocator {
    fn locate(&self, image: &ContentHash) -> AdjacentLocation {
        let Ok(Some(best)) = self.0.best_path(image) else {
            return AdjacentLocation::Offline { volume_id: None };
        };
        if !best.online {
            return AdjacentLocation::Offline {
                volume_id: Some(best.row.volume_id),
            };
        }
        let Some(mount) = best.mount_point.as_deref() else {
            return AdjacentLocation::Offline {
                volume_id: Some(best.row.volume_id),
            };
        };
        let image_path: PathBuf = PathBuf::from(mount).join(&best.row.rel_path);
        let read_only = self
            .0
            .volume(&best.row.volume_id)
            .ok()
            .flatten()
            .map(|v| v.read_only)
            .unwrap_or(true);
        if read_only {
            AdjacentLocation::Unwritable {
                image_path: Some(image_path),
                volume_id: Some(best.row.volume_id),
            }
        } else {
            AdjacentLocation::Writable { image_path }
        }
    }
}
