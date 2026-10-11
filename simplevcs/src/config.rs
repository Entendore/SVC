use crate::error::VcsResult;
use crate::repo::Repo;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
pub struct GlobalConfig {
    pub user: Option<UserConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UserConfig {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl GlobalConfig {
    pub fn load() -> Self {
        if let Some(config_path) = Self::get_global_config_path() {
            if config_path.exists() {
                if let Ok(contents) = std::fs::read_to_string(&config_path) {
                    return toml::from_str(&contents).unwrap_or_default();
                }
            }
        }
        Self::default()
    }

    fn get_global_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("simplevcs").join("config.toml"))
    }

    pub fn resolve_author(repo: &Repo, cli_arg: Option<&str>) -> VcsResult<String> {
        if let Some(a) = cli_arg { return Ok(a.to_string()); }
        if let Some(local) = repo.db.get("user.author")? {
            return Ok(String::from_utf8_lossy(&local).to_string());
        }
        let global = Self::load();
        if let Some(user) = global.user {
            if let Some(name) = user.name { return Ok(name); }
        }
        Ok("Unknown".to_string())
    }
}
