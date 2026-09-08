//! Cloud sync services (Phase 8).
//!
//! - WebDAV: simple PUT/GET of `aag.db` (Step 3).
//! - S3:     AWS SigV4 signed PUT/GET/HEAD (Step 4).

pub mod s3;
pub mod webdav;
