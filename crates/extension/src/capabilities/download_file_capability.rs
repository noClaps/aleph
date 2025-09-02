use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DownloadFileCapability {
    pub host: String,
    pub path: Vec<String>,
}

impl DownloadFileCapability {
    /// Returns whether the capability allows downloading a file from the given URL.
    pub fn allows(&self, url: &Url) -> bool {
        let Some(desired_host) = url.host_str() else {
            return false;
        };

        let Some(desired_path) = url.path_segments() else {
            return false;
        };
        let desired_path = desired_path.collect::<Vec<_>>();

        if self.host != desired_host && self.host != "*" {
            return false;
        }

        for (ix, path_segment) in self.path.iter().enumerate() {
            if path_segment == "**" {
                return true;
            }

            if ix >= desired_path.len() {
                return false;
            }

            if path_segment != "*" && path_segment != desired_path[ix] {
                return false;
            }
        }

        if self.path.len() < desired_path.len() {
            return false;
        }

        true
    }
}
