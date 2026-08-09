//! Everything that changes system state: privileged sessions, `apt-get`
//! subprocesses, and atomic edits to files under `/etc/apt`.

pub mod apt;
pub mod edit;
pub mod privileges;
