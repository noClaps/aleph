use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;
use url::Url;

use git::{
    BuildCommitPermalinkParams, BuildPermalinkParams, GitHostingProvider, ParsedGitRemote,
    PullRequest, RemoteUrl,
};

fn pull_request_regex() -> &'static Regex {
    static PULL_REQUEST_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        // This matches Bitbucket PR reference pattern: (pull request #xxx)
        Regex::new(r"\(pull request #(\d+)\)").unwrap()
    });
    &PULL_REQUEST_REGEX
}

pub struct Bitbucket {
    name: String,
    base_url: Url,
}

impl Bitbucket {
    pub fn new(name: impl Into<String>, base_url: Url) -> Self {
        Self {
            name: name.into(),
            base_url,
        }
    }

    pub fn public_instance() -> Self {
        Self::new("Bitbucket", Url::parse("https://bitbucket.org").unwrap())
    }
}

impl GitHostingProvider for Bitbucket {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn base_url(&self) -> Url {
        self.base_url.clone()
    }

    fn supports_avatars(&self) -> bool {
        false
    }

    fn format_line_number(&self, line: u32) -> String {
        format!("lines-{line}")
    }

    fn format_line_numbers(&self, start_line: u32, end_line: u32) -> String {
        format!("lines-{start_line}:{end_line}")
    }

    fn parse_remote_url(&self, url: &str) -> Option<ParsedGitRemote> {
        let url = RemoteUrl::from_str(url).ok()?;

        let host = url.host_str()?;
        if host != "bitbucket.org" {
            return None;
        }

        let mut path_segments = url.path_segments()?;
        let owner = path_segments.next()?;
        let repo = path_segments.next()?.trim_end_matches(".git");

        Some(ParsedGitRemote {
            owner: owner.into(),
            repo: repo.into(),
        })
    }

    fn build_commit_permalink(
        &self,
        remote: &ParsedGitRemote,
        params: BuildCommitPermalinkParams,
    ) -> Url {
        let BuildCommitPermalinkParams { sha } = params;
        let ParsedGitRemote { owner, repo } = remote;

        self.base_url()
            .join(&format!("{owner}/{repo}/commits/{sha}"))
            .unwrap()
    }

    fn build_permalink(&self, remote: ParsedGitRemote, params: BuildPermalinkParams) -> Url {
        let ParsedGitRemote { owner, repo } = remote;
        let BuildPermalinkParams {
            sha,
            path,
            selection,
        } = params;

        let mut permalink = self
            .base_url()
            .join(&format!("{owner}/{repo}/src/{sha}/{path}"))
            .unwrap();
        permalink.set_fragment(
            selection
                .map(|selection| self.line_fragment(&selection))
                .as_deref(),
        );
        permalink
    }

    fn extract_pull_request(&self, remote: &ParsedGitRemote, message: &str) -> Option<PullRequest> {
        // Check first line of commit message for PR references
        let first_line = message.lines().next()?;

        // Try to match against our PR patterns
        let capture = pull_request_regex().captures(first_line)?;
        let number = capture.get(1)?.as_str().parse::<u32>().ok()?;

        // Construct the PR URL in Bitbucket format
        let mut url = self.base_url();
        let path = format!("/{}/{}/pull-requests/{}", remote.owner, remote.repo, number);
        url.set_path(&path);

        Some(PullRequest { number, url })
    }
}
