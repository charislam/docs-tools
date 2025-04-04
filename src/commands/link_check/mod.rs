use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use anyhow::Result;
use log::{debug, error, info};
use lychee_lib::{extract::Extractor, ClientBuilder, FileType, InputContent};
use url::{ParseError, Url};

mod utils;
mod visited;

use utils::{is_html, StartsWith as _};
use visited::Visited;

pub(crate) struct LinkChecker {
    base_url: Url,
    client: lychee_lib::Client,
    extractor: Extractor,
    visited: Arc<Visited>,
    successful_checks: Arc<AtomicUsize>,
    failed_checks: Arc<AtomicUsize>,
}

enum CheckResult {
    Success(Option<NextTargets>),
    Failure,
}

type NextTargets = Vec<Url>;

impl LinkChecker {
    pub(crate) fn new(base_url: impl AsRef<str>) -> Result<Self> {
        debug!("Creating LinkChecker with base: {}", base_url.as_ref());
        let base_url = Url::parse(base_url.as_ref())?;

        let client = ClientBuilder::default().client()?;
        let extractor = Extractor::default();
        let visited = Arc::new(Visited::default());

        let successful_checks = Arc::new(AtomicUsize::new(0));
        let failed_checks = Arc::new(AtomicUsize::new(0));

        Ok(Self {
            base_url,
            client,
            extractor,
            visited,
            successful_checks,
            failed_checks,
        })
    }

    pub(crate) async fn check(&self, start_url: impl AsRef<str>) -> Result<()> {
        let start_url = Url::parse(start_url.as_ref())?;
        if !start_url.origin().eq(&self.base_url.origin()) {
            error!("Start URL must be within the base URL domain");
            anyhow::bail!("Start URL must be within the base URL domain");
        }

        self.check_recursively(&start_url).await?;

        // Display summary
        let total_checks = self.successful_checks.load(Ordering::Relaxed)
            + self.failed_checks.load(Ordering::Relaxed);
        info!("\nLink Check Summary:");
        info!("Total links checked: {}", total_checks);
        info!(
            "Successful checks: {}",
            self.successful_checks.load(Ordering::Relaxed)
        );
        info!(
            "Failed checks: {}",
            self.failed_checks.load(Ordering::Relaxed)
        );

        // Check if any failures occurred
        if self.failed_checks.load(Ordering::Relaxed) > 0 {
            error!("Some links failed to check");
            anyhow::bail!("Some links failed to check");
        }

        Ok(())
    }

    async fn check_recursively(&self, url: &Url) -> Result<()> {
        debug!("Checking URL: {}", url.as_str());
        if !url.scheme().starts_with("http") {
            debug!("Skipping non-http(s) URL: {}", url.as_str());
            return Ok(());
        }
        if self.visited.mark_visited(url) {
            debug!("Skipping URL {} as already checked", url.as_str());
            return Ok(());
        }

        match url.starts_with(&self.base_url) && is_html(url, None) {
            true => {
                let CheckResult::Success(Some(next)) =
                    self.check_response_internal_maybe_html(url).await?
                else {
                    return Ok(());
                };

                for link in next {
                    Box::pin(self.check_recursively(&link)).await?;
                }
            }
            false => self.check_non_internal_html(url).await,
        }

        Ok(())
    }

    async fn check_response_internal_maybe_html(&self, url: &Url) -> Result<CheckResult> {
        let response = match reqwest::get(url.as_str()).await {
            Ok(response) => response,
            Err(e) => {
                error!("Failed to fetch {}: {}", url.as_str(), e);
                self.failed_checks.fetch_add(1, Ordering::Relaxed);
                return Ok(CheckResult::Failure);
            }
        };
        if !response.status().is_success() {
            error!("Failed to fetch {}: {}", url.as_str(), response.status());
            self.failed_checks.fetch_add(1, Ordering::Relaxed);
            return Ok(CheckResult::Failure);
        }
        info!("Successfully checked internal HTML link: {}", url.as_str());
        self.successful_checks.fetch_add(1, Ordering::Relaxed);

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok());
        if !is_html(url, content_type) {
            return Ok(CheckResult::Success(None));
        }

        let response_text = response.text().await;
        let Ok(response_text) = response_text else {
            let err_mess = format!("Failed to read response text from url: {}", url.as_str());
            error!("{err_mess}");
            anyhow::bail!("{err_mess}")
        };
        Ok(CheckResult::Success(Some(
            self.extract_links(url, &response_text),
        )))
    }

    fn extract_links(&self, curr_base: &Url, s: &str) -> NextTargets {
        let input = InputContent::from_string(s, FileType::Html);
        self.extractor
            .extract(&input)
            .iter()
            .filter_map(|raw_uri| {
                let link_str = &raw_uri.text;
                match Url::parse(link_str) {
                    Ok(url) => Some(url),
                    Err(ParseError::RelativeUrlWithoutBase) => curr_base.join(link_str).ok(),
                    Err(_) => {
                        error!("Error parsing URL from raw URI: {}", link_str);
                        None
                    }
                }
            })
            .collect()
    }

    async fn check_non_internal_html(&self, url: &Url) {
        match self.client.check(url.as_str()).await {
            Ok(response) => {
                if !response.status().is_success() {
                    error!(
                        "Link check failed for {}: {}",
                        url.as_str(),
                        response.status()
                    );
                    self.failed_checks.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.successful_checks.fetch_add(1, Ordering::Relaxed);
                    info!("Successfully checked link: {}", url.as_str());
                }
            }
            Err(e) => {
                error!("Failed to check link {}: {}", url.as_str(), e);
                self.failed_checks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
