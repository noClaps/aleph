use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProcessExecCapability {
    /// The command to execute.
    pub command: String,
    /// The arguments to pass to the command. Use `*` for a single wildcard argument.
    /// If the last element is `**`, then any trailing arguments are allowed.
    pub args: Vec<String>,
}

impl ProcessExecCapability {
    /// Returns whether the capability allows the given command and arguments.
    pub fn allows(
        &self,
        desired_command: &str,
        desired_args: &[impl AsRef<str> + std::fmt::Debug],
    ) -> bool {
        if self.command != desired_command && self.command != "*" {
            return false;
        }

        for (ix, arg) in self.args.iter().enumerate() {
            if arg == "**" {
                return true;
            }

            if ix >= desired_args.len() {
                return false;
            }

            if arg != "*" && arg != desired_args[ix].as_ref() {
                return false;
            }
        }

        if self.args.len() < desired_args.len() {
            return false;
        }

        true
    }
}
