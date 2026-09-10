use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const OWNER: &str = "mycode2025-ui";
const REPOSITORY: &str = "PCAN-Explorer10";
const GITEE_LATEST_API: &str =
    "https://gitee.com/api/v5/repos/mycode2025-ui/PCAN-Explorer10/releases/latest";
const GITHUB_LATEST_API: &str =
    "https://api.github.com/repos/mycode2025-ui/PCAN-Explorer10/releases/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateInfo {
    pub version: String,
    pub notes: String,
    pub gitee_download: String,
    pub github_download: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckResult {
    Available(UpdateInfo),
    Current { latest: String },
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Debug, Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Clone, Copy)]
enum Source {
    Gitee,
    Github,
}

pub(crate) fn check(current: &str) -> Result<CheckResult, String> {
    // Query both mirrors independently. A healthy but stale Gitee response must
    // never hide a newer GitHub release (and vice versa).
    let (gitee, github) = std::thread::scope(|scope| {
        let gitee = scope.spawn(|| check_source(current, GITEE_LATEST_API, Source::Gitee));
        let github = scope.spawn(|| check_source(current, GITHUB_LATEST_API, Source::Github));
        (
            gitee
                .join()
                .unwrap_or_else(|_| Err("Gitee 检查线程异常".into())),
            github
                .join()
                .unwrap_or_else(|_| Err("GitHub 检查线程异常".into())),
        )
    });
    let mut result = merge_results(gitee, github)?;
    if let CheckResult::Available(info) = &mut result {
        populate_reachable_mirrors(info);
    }
    Ok(result)
}

fn populate_reachable_mirrors(info: &mut UpdateInfo) {
    if info.gitee_download.is_empty() {
        let candidate = expected_download(Source::Gitee, &info.version);
        if download_exists(&candidate) {
            info.gitee_download = candidate;
        }
    }
    if info.github_download.is_empty() {
        let candidate = expected_download(Source::Github, &info.version);
        if download_exists(&candidate) {
            info.github_download = candidate;
        }
    }
}

fn expected_download(source: Source, version: &str) -> String {
    let tag = format!("v{version}");
    let installer = format!("PCAN-Explorer10-Setup-{version}.exe");
    match source {
        Source::Gitee => {
            format!("https://gitee.com/{OWNER}/{REPOSITORY}/releases/download/{tag}/{installer}")
        }
        Source::Github => {
            format!("https://github.com/{OWNER}/{REPOSITORY}/releases/download/{tag}/{installer}")
        }
    }
}

fn download_exists(url: &str) -> bool {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(7)))
        .https_only(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build();
    ureq::Agent::new_with_config(config)
        .get(url)
        .header("Range", "bytes=0-0")
        .header(
            "User-Agent",
            format!("PCAN-Explorer10/{}", crate::product_version::current()),
        )
        .call()
        .is_ok()
}

fn merge_results(
    gitee: Result<CheckResult, String>,
    github: Result<CheckResult, String>,
) -> Result<CheckResult, String> {
    match (gitee, github) {
        (Ok(gitee), Ok(github)) => merge_successful_results(gitee, github),
        (Ok(result), Err(_)) | (Err(_), Ok(result)) => Ok(result),
        (Err(gitee_error), Err(github_error)) => {
            Err(format!("Gitee: {gitee_error}; GitHub: {github_error}"))
        }
    }
}

fn merge_successful_results(
    gitee: CheckResult,
    github: CheckResult,
) -> Result<CheckResult, String> {
    match (gitee, github) {
        (CheckResult::Available(mut gitee), CheckResult::Available(github)) => {
            let gitee_version = parse_version(&gitee.version)?;
            let github_version = parse_version(&github.version)?;
            if github_version > gitee_version {
                return Ok(CheckResult::Available(github));
            }
            if github_version == gitee_version {
                gitee.github_download = github.github_download;
                if gitee.notes.is_empty() {
                    gitee.notes = github.notes;
                }
            }
            Ok(CheckResult::Available(gitee))
        }
        (CheckResult::Available(info), CheckResult::Current { .. })
        | (CheckResult::Current { .. }, CheckResult::Available(info)) => {
            Ok(CheckResult::Available(info))
        }
        (
            CheckResult::Current {
                latest: gitee_latest,
            },
            CheckResult::Current {
                latest: github_latest,
            },
        ) => {
            let latest = if parse_version(&github_latest)? > parse_version(&gitee_latest)? {
                github_latest
            } else {
                gitee_latest
            };
            Ok(CheckResult::Current { latest })
        }
    }
}

fn check_source(current: &str, url: &str, source: Source) -> Result<CheckResult, String> {
    evaluate_release(current, fetch_release(url)?, source)
}

fn fetch_release(url: &str) -> Result<ApiRelease, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .https_only(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(url)
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            format!("PCAN-Explorer10/{}", crate::product_version::current()),
        )
        .call()
        .map_err(|error| error.to_string())?;
    response
        .body_mut()
        .read_json::<ApiRelease>()
        .map_err(|error| error.to_string())
}

fn evaluate_release(
    current: &str,
    release: ApiRelease,
    source: Source,
) -> Result<CheckResult, String> {
    let current_version = parse_version(current)?;
    let latest_version = parse_version(&release.tag_name)?;
    let display_version = latest_version.to_string();
    if latest_version <= current_version {
        return Ok(CheckResult::Current {
            latest: display_version,
        });
    }

    let asset = select_installer(&release.assets)
        .ok_or_else(|| format!("{} 未包含 Windows 安装包", release.tag_name))?;
    let (gitee_download, github_download) = match source {
        Source::Gitee => (asset.browser_download_url.clone(), String::new()),
        Source::Github => (String::new(), asset.browser_download_url.clone()),
    };

    Ok(CheckResult::Available(UpdateInfo {
        version: display_version,
        notes: compact_notes(&release.body),
        gitee_download,
        github_download,
    }))
}

fn parse_version(value: &str) -> Result<Version, String> {
    Version::parse(value.trim().trim_start_matches(['v', 'V']))
        .map_err(|error| format!("无法解析版本 {value}: {error}"))
}

fn select_installer(assets: &[ApiAsset]) -> Option<&ApiAsset> {
    assets
        .iter()
        .find(|asset| {
            let name = asset.name.to_ascii_lowercase();
            (name.starts_with("pcan-explorer10-setup-") || name.starts_with("pcanwork-setup-"))
                && name.ends_with(".exe")
        })
        .or_else(|| {
            assets
                .iter()
                .find(|asset| asset.name.to_ascii_lowercase().ends_with(".exe"))
        })
}

fn compact_notes(notes: &str) -> String {
    let items = notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| {
            !line.starts_with("安装包：")
                && !line.starts_with("SHA-256：")
                && !line.starts_with("签名状态：")
        })
        .map(|line| {
            line.strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line)
        })
        .take(3)
        .map(|line| format!("• {line}"))
        .collect::<Vec<_>>();
    let text = items.join("\n");
    if text.chars().count() <= 220 {
        return text;
    }
    let mut shortened = text.chars().take(217).collect::<String>();
    shortened.push_str("...");
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[(&str, &str)]) -> ApiRelease {
        ApiRelease {
            tag_name: tag.to_string(),
            body: "修复问题\n改进体验".to_string(),
            assets: assets
                .iter()
                .map(|(name, url)| ApiAsset {
                    name: (*name).to_string(),
                    browser_download_url: (*url).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn newer_version_uses_only_the_confirmed_source_asset() {
        let result = evaluate_release(
            "0.1.24",
            release(
                "v0.1.25",
                &[(
                    "PCAN-Explorer10-Setup-0.1.25.exe",
                    "https://gitee.test/setup.exe",
                )],
            ),
            Source::Gitee,
        )
        .unwrap();
        let CheckResult::Available(info) = result else {
            panic!("expected available update");
        };
        assert_eq!(info.version, "0.1.25");
        assert_eq!(info.gitee_download, "https://gitee.test/setup.exe");
        assert!(info.github_download.is_empty());
    }

    #[test]
    fn github_newer_release_is_not_hidden_by_current_gitee_release() {
        let gitee = evaluate_release("0.4.7", release("v0.4.7", &[]), Source::Gitee);
        let github = evaluate_release(
            "0.4.7",
            release(
                "v0.4.8",
                &[(
                    "PCAN-Explorer10-Setup-0.4.8.exe",
                    "https://github.test/setup.exe",
                )],
            ),
            Source::Github,
        );
        let CheckResult::Available(info) = merge_results(gitee, github).unwrap() else {
            panic!("expected GitHub update");
        };
        assert_eq!(info.version, "0.4.8");
        assert!(info.gitee_download.is_empty());
        assert_eq!(info.github_download, "https://github.test/setup.exe");
    }

    #[test]
    fn equal_release_versions_combine_both_confirmed_downloads() {
        let gitee = evaluate_release(
            "0.4.7",
            release(
                "v0.4.8",
                &[(
                    "PCAN-Explorer10-Setup-0.4.8.exe",
                    "https://gitee.test/setup.exe",
                )],
            ),
            Source::Gitee,
        );
        let github = evaluate_release(
            "0.4.7",
            release(
                "v0.4.8",
                &[(
                    "PCAN-Explorer10-Setup-0.4.8.exe",
                    "https://github.test/setup.exe",
                )],
            ),
            Source::Github,
        );
        let CheckResult::Available(info) = merge_results(gitee, github).unwrap() else {
            panic!("expected combined update");
        };
        assert_eq!(info.gitee_download, "https://gitee.test/setup.exe");
        assert_eq!(info.github_download, "https://github.test/setup.exe");
    }

    #[test]
    fn expected_download_urls_use_the_product_release_convention() {
        assert_eq!(
            expected_download(Source::Github, "0.4.9"),
            "https://github.com/mycode2025-ui/PCAN-Explorer10/releases/download/v0.4.9/PCAN-Explorer10-Setup-0.4.9.exe"
        );
        assert_eq!(
            expected_download(Source::Gitee, "0.4.9"),
            "https://gitee.com/mycode2025-ui/PCAN-Explorer10/releases/download/v0.4.9/PCAN-Explorer10-Setup-0.4.9.exe"
        );
    }

    #[test]
    fn equal_or_older_release_is_current() {
        assert!(matches!(
            evaluate_release("0.1.24", release("v0.1.24", &[]), Source::Github).unwrap(),
            CheckResult::Current { .. }
        ));
    }

    #[test]
    fn patch_versions_are_compared_numerically_not_lexically() {
        assert!(matches!(
            evaluate_release("0.3.20", release("v0.3.11", &[]), Source::Github).unwrap(),
            CheckResult::Current { latest } if latest == "0.3.11"
        ));
        assert!(matches!(
            evaluate_release("0.3.2", release("v0.3.11", &[("PCAN-Explorer10-Setup-0.3.11.exe", "setup")]), Source::Github).unwrap(),
            CheckResult::Available(info) if info.version == "0.3.11"
        ));
    }

    #[test]
    fn installer_selection_prefers_named_setup() {
        let release = release(
            "v1.0.0",
            &[
                ("helper.exe", "one"),
                ("PCAN-Explorer10-Setup-1.0.0.exe", "two"),
            ],
        );
        assert_eq!(
            select_installer(&release.assets)
                .unwrap()
                .browser_download_url,
            "two"
        );
    }

    #[test]
    fn release_notes_keep_utf8_and_strip_markdown_metadata() {
        let notes = "## PCAN-Explorer10 v0.3.25\n\n- 改进 PCAN-USB FD 初始化兼容性。\n- PCAN ↔ ZLG 完成双向实机验证。\n- 每档波特率载荷校验正确。\n- 第四条不在弹窗显示。\n\n安装包：PCAN-Explorer10-Setup.exe\nSHA-256：ABC";
        assert_eq!(
            compact_notes(notes),
            "• 改进 PCAN-USB FD 初始化兼容性。\n• PCAN ↔ ZLG 完成双向实机验证。\n• 每档波特率载荷校验正确。"
        );
    }

    #[test]
    #[ignore = "requires public release APIs"]
    fn live_release_api_returns_downloadable_update() {
        let CheckResult::Available(info) = check("0.0.0").unwrap() else {
            panic!("expected the published release to be newer than 0.0.0");
        };
        assert!(info.gitee_download.starts_with("https://"));
        assert!(info.github_download.starts_with("https://"));
    }
}
