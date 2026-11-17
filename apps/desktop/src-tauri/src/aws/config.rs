//! Reads the AWS CLI profiles of this computer from `~/.aws/config` (never `credentials`), and
//! works out who assumes a role from whom: the master profiles and the profiles that depend on
//! them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// One `[profile x]` (or `[default]`) section, with only the keys the AWS screen uses.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Profile {
    pub name: String,
    pub region: Option<String>,
    pub role_arn: Option<String>,
    pub source_profile: Option<String>,
    /// `Environment`, `Ec2InstanceMetadata` or `EcsContainer`: assumes a role without a profile.
    pub credential_source: Option<String>,
    /// How this profile signs in by itself: `sso`, `login` (`aws login`), `process` or `keys`
    /// (static keys kept by the CLI, which Vessel never reads).
    pub sign_in: Option<String>,
    /// The account, when the role ARN (or SSO settings) say it.
    pub account: Option<String>,
    /// Profiles that assume a role from this one, directly.
    pub dependents: Vec<String>,
    /// The root of this profile's `source_profile` chain (itself for a root).
    pub master: String,
    /// The chain is broken: a missing `source_profile` or a loop. Says why.
    pub problem: Option<String>,
}

/// Where the CLI reads its config: `AWS_CONFIG_FILE`, or `~/.aws/config`.
pub fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AWS_CONFIG_FILE")
        && !p.trim().is_empty()
    {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".aws").join("config"))
}

/// Profiles of the config file at `path`; an absent file is no profiles.
pub fn load(path: &std::path::Path) -> std::io::Result<Vec<Profile>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Parses the INI text of an AWS config file into profiles, linked into their role graph.
pub fn parse(text: &str) -> Vec<Profile> {
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut last_key: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') {
            last_key = None;
            current = trimmed
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|name| {
                    let name = name.trim();
                    if name == "default" {
                        Some("default".to_string())
                    } else {
                        name.strip_prefix("profile")
                            .filter(|rest| rest.starts_with(char::is_whitespace))
                            .map(|rest| rest.trim().to_string())
                            .filter(|n| !n.is_empty())
                    }
                });
            if let Some(name) = &current {
                sections.entry(name.clone()).or_default();
            }
            continue;
        }
        let Some(section) = current.as_ref() else {
            continue;
        };
        // Indented lines continue a nested value (`s3 =` blocks); their keys are not ours.
        if line.starts_with(char::is_whitespace) && last_key.is_some() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim().to_ascii_lowercase();
            let value = strip_comment(v.trim()).to_string();
            last_key = Some(key.clone());
            sections
                .entry(section.clone())
                .or_default()
                .insert(key, value);
        }
    }

    let mut profiles: BTreeMap<String, Profile> = sections
        .into_iter()
        .map(|(name, kv)| {
            let get = |k: &str| kv.get(k).filter(|v| !v.is_empty()).cloned();
            let role_arn = get("role_arn");
            let sign_in = if get("sso_session").is_some() || get("sso_start_url").is_some() {
                Some("sso")
            } else if get("login_session").is_some() {
                Some("login")
            } else if get("credential_process").is_some() {
                Some("process")
            } else if role_arn.is_none() {
                Some("keys")
            } else {
                None
            };
            let account = role_arn
                .as_deref()
                .and_then(account_of_arn)
                .or_else(|| get("sso_account_id"));
            let profile = Profile {
                name: name.clone(),
                region: get("region"),
                source_profile: get("source_profile"),
                credential_source: get("credential_source"),
                sign_in: sign_in.map(str::to_string),
                account,
                role_arn,
                dependents: Vec::new(),
                master: name.clone(),
                problem: None,
            };
            (name, profile)
        })
        .collect();

    // Walk each chain up to its root, stopping at loops and missing parents.
    let names: Vec<String> = profiles.keys().cloned().collect();
    for name in &names {
        let mut seen = vec![name.clone()];
        let mut at = name.clone();
        let mut problem = None;
        loop {
            let parent = profiles[&at].source_profile.clone();
            match parent {
                // A profile that names itself signs in with its own keys and then assumes the
                // role: the CLI allows it, and it is a root.
                Some(p) if p == at => break,
                Some(p) if !profiles.contains_key(&p) => {
                    problem = Some(format!(
                        "source_profile \"{p}\" não existe no arquivo de configuração."
                    ));
                    break;
                }
                Some(p) if seen.contains(&p) => {
                    problem = Some(format!("A cadeia de source_profile volta para \"{p}\"."));
                    break;
                }
                Some(p) => {
                    seen.push(p.clone());
                    at = p;
                }
                None => break,
            }
        }
        let entry = profiles.get_mut(name).expect("listed above");
        entry.master = at;
        entry.problem = problem;
    }
    for name in &names {
        if let Some(parent) = profiles[name].source_profile.clone()
            && parent != *name
            && let Some(p) = profiles.get_mut(&parent)
        {
            p.dependents.push(name.clone());
        }
    }
    profiles.into_values().collect()
}

/// The profile most others depend on: the best guess for the master when none was chosen.
pub fn default_master(profiles: &[Profile]) -> Option<String> {
    let count = |name: &str| {
        profiles
            .iter()
            .filter(|p| p.master == name && p.name != name)
            .count()
    };
    profiles
        .iter()
        .filter(|p| p.master == p.name)
        .max_by_key(|p| (count(&p.name), p.name == "default"))
        .map(|p| p.name.clone())
}

/// The profiles that run through `master`: itself and every profile whose chain ends there.
pub fn family<'a>(profiles: &'a [Profile], master: &str) -> Vec<&'a Profile> {
    profiles.iter().filter(|p| p.master == master).collect()
}

fn account_of_arn(arn: &str) -> Option<String> {
    // arn:aws:iam::123456789012:role/name
    let account = arn.split(':').nth(4)?;
    (account.len() == 12 && account.bytes().all(|b| b.is_ascii_digit()))
        .then(|| account.to_string())
}

fn strip_comment(v: &str) -> &str {
    // The CLI treats " #" and " ;" after a value as a comment.
    let cut = [" #", " ;", "\t#", "\t;"]
        .iter()
        .filter_map(|m| v.find(m))
        .min();
    match cut {
        Some(i) => v[..i].trim_end(),
        None => v,
    }
}

/// Profile names are passed to the CLI as arguments: only names that exist in the config are
/// accepted, and only characters a profile name can have.
pub fn valid_profile<'a>(profiles: &'a [Profile], name: &str) -> Option<&'a Profile> {
    let clean = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.@+=,/".contains(c));
    if !clean {
        return None;
    }
    profiles.iter().find(|p| p.name == name)
}

/// AWS region names (`us-east-1`, `ap-southeast-3`, `us-gov-west-1`).
pub fn valid_region(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 32
        && r.split('-').count() >= 3
        && r.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[default]
region = us-east-1

[profile master]
login_session = arn:aws:iam::111111111111:user/me
region = sa-east-1

[profile prod]
role_arn = arn:aws:iam::222222222222:role/Admin
source_profile = master
region = us-east-1 # main region

[profile prod-readonly]
role_arn = arn:aws:iam::222222222222:role/ReadOnly
source_profile = prod

[profile dev]
role_arn = arn:aws:iam::333333333333:role/Dev
source_profile = master
s3 =
  max_concurrent_requests = 20

[profile sso-team]
sso_session = corp
sso_account_id = 444444444444
sso_role_name = Team

[sso-session corp]
sso_start_url = https://example.awsapps.com/start

[profile broken]
role_arn = arn:aws:iam::555555555555:role/X
source_profile = nowhere

[profile loop-a]
role_arn = arn:aws:iam::666666666666:role/A
source_profile = loop-b

[profile loop-b]
role_arn = arn:aws:iam::666666666666:role/B
source_profile = loop-a

[profile self]
role_arn = arn:aws:iam::777777777777:role/Self
source_profile = self
"#;

    fn get<'a>(p: &'a [Profile], n: &str) -> &'a Profile {
        p.iter().find(|x| x.name == n).unwrap()
    }

    #[test]
    fn reads_profiles_and_their_settings() {
        let p = parse(SAMPLE);
        let names: Vec<_> = p.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "broken",
                "default",
                "dev",
                "loop-a",
                "loop-b",
                "master",
                "prod",
                "prod-readonly",
                "self",
                "sso-team"
            ]
        );
        assert_eq!(
            get(&p, "prod").region.as_deref(),
            Some("us-east-1"),
            "inline comment dropped"
        );
        assert_eq!(get(&p, "prod").account.as_deref(), Some("222222222222"));
        assert_eq!(get(&p, "master").sign_in.as_deref(), Some("login"));
        assert_eq!(get(&p, "sso-team").sign_in.as_deref(), Some("sso"));
        assert_eq!(get(&p, "sso-team").account.as_deref(), Some("444444444444"));
        assert_eq!(get(&p, "default").sign_in.as_deref(), Some("keys"));
        assert_eq!(
            get(&p, "prod").sign_in,
            None,
            "a role profile signs in through its source"
        );
        // The nested s3 block does not leak keys into the profile.
        assert_eq!(get(&p, "dev").region, None);
    }

    #[test]
    fn builds_the_assume_role_graph() {
        let p = parse(SAMPLE);
        assert_eq!(get(&p, "prod-readonly").master, "master", "two hops up");
        assert_eq!(get(&p, "prod").master, "master");
        assert_eq!(get(&p, "master").dependents, ["dev", "prod"]);
        assert_eq!(get(&p, "prod").dependents, ["prod-readonly"]);
        assert_eq!(get(&p, "self").master, "self");
        assert!(get(&p, "self").problem.is_none());
        assert!(
            get(&p, "broken")
                .problem
                .as_deref()
                .unwrap()
                .contains("nowhere")
        );
        assert!(
            get(&p, "loop-a")
                .problem
                .as_deref()
                .unwrap()
                .contains("volta")
        );
        assert_eq!(default_master(&p).as_deref(), Some("master"));
        let fam: Vec<_> = family(&p, "master")
            .iter()
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(fam, ["dev", "master", "prod", "prod-readonly"]);
    }

    #[test]
    fn only_existing_clean_profile_names_reach_the_cli() {
        let p = parse(SAMPLE);
        assert!(valid_profile(&p, "prod").is_some());
        assert!(valid_profile(&p, "nope").is_none());
        assert!(valid_profile(&p, "--debug").is_none());
        assert!(valid_profile(&p, "prod; rm -rf /").is_none());
        assert!(valid_region("sa-east-1") && valid_region("us-gov-west-1"));
        assert!(!valid_region("us-east-1 --x") && !valid_region("") && !valid_region("local"));
    }

    #[test]
    fn empty_or_odd_files_do_not_panic() {
        assert!(parse("").is_empty());
        assert!(parse("[profile ]\nregion=x\n[profile]\n=\nkey").is_empty());
        let p = parse("[profile a]\nsource_profile = a\nrole_arn = arn:aws:iam::1:role/x");
        assert_eq!(p[0].account, None, "short account ids are not accounts");
    }
}
