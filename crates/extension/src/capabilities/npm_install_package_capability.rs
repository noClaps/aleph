use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NpmInstallPackageCapability {
    pub package: String,
}

impl NpmInstallPackageCapability {
    /// Returns whether the capability allows installing the given NPM package.
    pub fn allows(&self, package: &str) -> bool {
        self.package == "*" || self.package == package
    }
}
