//! gdirect's engine for the mobile app: the upstream modules without the
//! terminal UI, the rclone mount and the libaria2 binding. Downloads are
//! done by [`download`], in pure Rust, so nothing here needs a C++ toolchain.
pub mod api;
pub mod auth;
pub mod broker;
pub mod download;
pub mod drive;
pub mod netif;
pub mod ranges;
pub mod route;
pub mod scan;
pub mod store;
#[cfg(test)]
mod test_network;
pub mod upload;
