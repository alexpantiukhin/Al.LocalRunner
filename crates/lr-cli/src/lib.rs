//! The `localrunner` binary's library: the composition root (`build`), the role startup
//! (`roles`) and the commands (`commands`).

pub mod admin;
pub mod broker_admin;
pub mod build;
pub mod commands;
pub mod control;
pub mod dashboard;
pub mod doctor;
pub mod facts;
pub mod fs_files;
pub mod git_repo;
pub mod host;
pub mod init;
pub mod ipc_client;
pub mod join;
pub mod net;
pub mod os_service;
pub mod paths;
pub mod roles;
pub mod runner_config;
pub mod secrets;
pub mod state_dir;
