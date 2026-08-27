//! Typed GStreamer media pipelines for direct `linkctl` operations.

#[cfg(feature = "gstreamer")]
mod gst_backend;

#[cfg(feature = "gstreamer")]
pub use gst_backend::*;

#[cfg(not(feature = "gstreamer"))]
use link_core::{ErrorKind, LinkError};

/// Report that this build intentionally excludes media support.
#[cfg(not(feature = "gstreamer"))]
pub fn unavailable() -> LinkError {
    LinkError::new(
        ErrorKind::CapabilityUnsupported,
        "this build does not include the GStreamer media backend",
    )
}
