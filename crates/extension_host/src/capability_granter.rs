use std::sync::Arc;

use anyhow::{Result, bail};
use extension::{ExtensionCapability, ExtensionManifest};
use url::Url;

pub struct CapabilityGranter {
    granted_capabilities: Vec<ExtensionCapability>,
    manifest: Arc<ExtensionManifest>,
}

impl CapabilityGranter {
    pub fn new(
        granted_capabilities: Vec<ExtensionCapability>,
        manifest: Arc<ExtensionManifest>,
    ) -> Self {
        Self {
            granted_capabilities,
            manifest,
        }
    }

    pub fn grant_exec(
        &self,
        desired_command: &str,
        desired_args: &[impl AsRef<str> + std::fmt::Debug],
    ) -> Result<()> {
        self.manifest.allow_exec(desired_command, desired_args)?;

        let is_allowed = self
            .granted_capabilities
            .iter()
            .any(|capability| match capability {
                ExtensionCapability::ProcessExec(capability) => {
                    capability.allows(desired_command, desired_args)
                }
                _ => false,
            });

        if !is_allowed {
            bail!(
                "capability for process:exec {desired_command} {desired_args:?} is not granted by the extension host",
            );
        }

        Ok(())
    }

    pub fn grant_download_file(&self, desired_url: &Url) -> Result<()> {
        let is_allowed = self
            .granted_capabilities
            .iter()
            .any(|capability| match capability {
                ExtensionCapability::DownloadFile(capability) => capability.allows(desired_url),
                _ => false,
            });

        if !is_allowed {
            bail!(
                "capability for download_file {desired_url} is not granted by the extension host",
            );
        }

        Ok(())
    }

    pub fn grant_npm_install_package(&self, package_name: &str) -> Result<()> {
        let is_allowed = self
            .granted_capabilities
            .iter()
            .any(|capability| match capability {
                ExtensionCapability::NpmInstallPackage(capability) => {
                    capability.allows(package_name)
                }
                _ => false,
            });

        if !is_allowed {
            bail!("capability for npm:install {package_name} is not granted by the extension host",);
        }

        Ok(())
    }
}
