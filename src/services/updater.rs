use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Debug, Clone)]
pub struct UpdateCheckResult {
    pub has_update: bool,
    pub latest_tag: String,
    pub release_url: String,
    pub message: String,
}

pub fn check_github_release(repo_slug: &str, current_version: &str) -> Result<UpdateCheckResult, String> {
    let url = format!("https://api.github.com/repos/{repo_slug}/releases/latest");
    let client = Client::builder()
        .user_agent("TorrServerUI-Portable")
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP ошибка: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("Сетевая ошибка проверки обновлений: {e}"))?;

    if resp.status().as_u16() == 404 {
        return Ok(UpdateCheckResult {
            has_update: false,
            latest_tag: format!("v{current_version}"),
            release_url: format!("https://github.com/{repo_slug}/releases"),
            message: "Опубликованные релизы пока не найдены.".into(),
        });
    }
    if !resp.status().is_success() {
        return Err(format!("GitHub API ответил HTTP {}", resp.status()));
    }

    let rel: GithubRelease = resp
        .json()
        .map_err(|e| format!("Ошибка разбора ответа GitHub: {e}"))?;
    let clean_latest = rel.tag_name.trim_start_matches('v').trim();
    let has_update = clean_latest != current_version.trim();
    let msg = if has_update {
        format!("Доступна новая версия {} (текущая v{})", rel.tag_name, current_version)
    } else {
        format!("Установлена актуальная версия v{current_version}")
    };

    Ok(UpdateCheckResult {
        has_update,
        latest_tag: rel.tag_name,
        release_url: rel.html_url,
        message: msg,
    })
}
