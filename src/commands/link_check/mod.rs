use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::Result;
use futures::{stream, StreamExt};
use log::{debug, error, info};
use lychee_lib::{extract::Extractor, FileType, InputContent};
use url::{ParseError, Url};

mod progress;
mod utils;
mod visited;

use progress::ProgressBar;
use utils::{get_origin, is_html, StartsWith as _};
use visited::Visited;

#[derive(Clone)]
pub(crate) struct LinkChecker {
    /// The base URL used to determine whether a link is internal (should be
    /// recursively checked) or external
    base_url: Url,
    /// Client for the link checker library
    lychee_client: Arc<lychee_lib::Client>,
    /// Client for raw HTTP requests
    reqwest_client: reqwest::Client,
    /// Extractor to extract HTML links from HTML documents
    extractor: Extractor,
    /// Links that have already been visited
    visited: Arc<Visited>,
    /// Number of successfully checked links
    successful_checks: Arc<AtomicUsize>,
    /// Number of link check failures
    failed_checks: Arc<AtomicUsize>,
    /// Whether to only check links that are internal
    internal_only: bool,
    /// Progress bar for CLI display
    progress_bar: Arc<Mutex<Option<ProgressBar>>>,
}

/// A URL to check along with information about where it came from
struct UrlWithReferrer {
    url: Url,
    referrer: Option<Url>,
}

enum CheckResult {
    Success(Option<NextTargets>),
    Failure,
}

type NextTargets = Vec<UrlWithReferrer>;

struct MaxConcurrency(usize);

impl std::ops::Deref for MaxConcurrency {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for MaxConcurrency {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

const HUMAN_USER_AGENT: &str =  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0 Safari/537.36";
const DEFAULT_USER_AGENT: &str = "docs-tools";

impl LinkChecker {
    pub(crate) fn new(
        base_url: impl AsRef<str>,
        internal_only: bool,
        human_agent: bool,
    ) -> Result<Self> {
        debug!("Creating LinkChecker with base: {}", base_url.as_ref());
        let base_url = Url::parse(base_url.as_ref())?;

        let user_agent = if human_agent {
            HUMAN_USER_AGENT
        } else {
            DEFAULT_USER_AGENT
        };
        let lychee_client = lychee_lib::ClientBuilder::builder()
            .user_agent(user_agent)
            .build()
            .client()?;
        let reqwest_client = reqwest::Client::builder()
            .user_agent(user_agent)
            .pool_idle_timeout(Some(Duration::from_secs(30)))
            .timeout(Duration::from_secs(30))
            .build()?;

        let extractor = Extractor::default();
        let visited = Arc::new(Visited::default());
        let successful_checks = Arc::new(AtomicUsize::new(0));
        let failed_checks = Arc::new(AtomicUsize::new(0));
        let progress_bar = Arc::new(Mutex::new(None));

        Ok(Self {
            base_url,
            lychee_client: Arc::new(lychee_client),
            reqwest_client,
            extractor,
            visited,
            successful_checks,
            failed_checks,
            internal_only,
            progress_bar,
        })
    }

    pub(crate) async fn check(&self, start_url: impl AsRef<str>) -> Result<()> {
        let start_url = Url::parse(start_url.as_ref())?;
        if !start_url.origin().eq(&self.base_url.origin()) {
            error!("Start URL must be within the base URL domain");
            anyhow::bail!("Start URL must be within the base URL domain");
        }

        let mut pb = ProgressBar::new();
        pb.init();
        {
            let mut pb_lock = self.progress_bar.lock().unwrap();
            *pb_lock = Some(pb);
        }

        let queue = Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(UrlWithReferrer {
            url: start_url,
            referrer: None,
        });
        self.run_queue(queue, MaxConcurrency(10)).await?;

        {
            let mut pb_lock = self.progress_bar.lock().unwrap();
            if let Some(mut pb) = pb_lock.take() {
                pb.finish();
            }
        }

        self.display_summary();
        self.fail_on_error()
    }

    async fn run_queue(
        &self,
        queue: Arc<Mutex<VecDeque<UrlWithReferrer>>>,
        max_concurrent: MaxConcurrency,
    ) -> Result<()> {
        loop {
            let batch: Vec<UrlWithReferrer> = {
                let mut queue_lock = queue.lock().unwrap();
                let mut batch = Vec::with_capacity(*max_concurrent);
                while let Some(url_with_referrer) = queue_lock.pop_front() {
                    batch.push(url_with_referrer);
                    if batch.len() >= *max_concurrent {
                        break;
                    }
                }
                batch
            };
            if batch.is_empty() {
                break;
            }

            let results = stream::iter(batch)
                .map(|url_with_referrer| {
                    let checker = self.clone();
                    let queue_clone = Arc::clone(&queue);
                    async move {
                        checker
                            .process_url_parallel(&url_with_referrer, queue_clone)
                            .await
                    }
                })
                .buffer_unordered(*max_concurrent)
                .collect::<Vec<Result<()>>>()
                .await;
            for result in results {
                result?;
            }
        }
        Ok(())
    }

    async fn process_url_parallel(
        &self,
        url_with_referrer: &UrlWithReferrer,
        queue: Arc<Mutex<VecDeque<UrlWithReferrer>>>,
    ) -> Result<()> {
        let url = &url_with_referrer.url;
        let referrer = &url_with_referrer.referrer;

        {
            let mut pb_lock = self.progress_bar.lock().unwrap();
            if let Some(pb) = pb_lock.as_mut() {
                pb.curr_checking(url)
            }
        }

        if !url.scheme().starts_with("http") {
            debug!("Skipping non-http(s) URL: {}", url.as_str());
            return Ok(());
        }

        if self.visited.mark_visited(url) {
            debug!("Skipping URL {} as already checked", url.as_str());
            return Ok(());
        }

        // If internal_only is true, skip non-internal URLs
        if self.internal_only && !url.starts_with(&self.base_url) {
            debug!(
                "Skipping external URL due to --internal-only flag: {}",
                url.as_str()
            );
            return Ok(());
        }

        match url.starts_with(&self.base_url) && is_html(url, None) {
            true => {
                let result = self
                    .check_response_internal_maybe_html(url, referrer.as_ref())
                    .await?;
                if let CheckResult::Success(Some(next)) = result {
                    let mut queue_lock = queue.lock().unwrap();
                    for next_url in next {
                        queue_lock.push_back(next_url);
                    }
                }
            }
            false => self.check_non_internal_html(url, referrer.as_ref()).await,
        }

        Ok(())
    }

    async fn check_response_internal_maybe_html(
        &self,
        url: &Url,
        referrer: Option<&Url>,
    ) -> Result<CheckResult> {
        let response = match self.reqwest_client.get(url.as_str()).send().await {
            Ok(response) => response,
            Err(e) => {
                if let Some(ref_url) = referrer {
                    error!(
                        "Failed to fetch {} (referrer: {}): {}",
                        url.as_str(),
                        ref_url.as_str(),
                        e
                    );
                } else {
                    error!("Failed to fetch {}: {}", url.as_str(), e);
                }
                self.failed_checks.fetch_add(1, Ordering::Relaxed);
                return Ok(CheckResult::Failure);
            }
        };
        if !response.status().is_success() {
            if let Some(ref_url) = referrer {
                error!(
                    "Failed to fetch {} (referrer: {}): {}",
                    url.as_str(),
                    ref_url.as_str(),
                    response.status()
                );
            } else {
                error!("Failed to fetch {}: {}", url.as_str(), response.status());
            }
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
                let parsed_url = match Url::parse(link_str) {
                    Ok(url) => Some(url),
                    Err(ParseError::RelativeUrlWithoutBase) => {
                        if let Some(stripped) = link_str.strip_prefix('/') {
                            let origin_url = get_origin(curr_base)?;
                            origin_url.join(stripped).ok()
                        } else {
                            curr_base.join(link_str).ok()
                        }
                    }
                    Err(_) => {
                        error!("Error parsing URL from raw URI: {}", link_str);
                        None
                    }
                };
                parsed_url.map(|url| UrlWithReferrer {
                    url,
                    referrer: Some(curr_base.clone()),
                })
            })
            // Cap path depth to avoid infinite recursion from self-referring pages
            .filter(|url_with_referrer| {
                const MAX_PATH_DEPTH: usize = 20;
                let path_segments: Vec<_> = url_with_referrer
                    .url
                    .path()
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .collect();
                if path_segments.len() > MAX_PATH_DEPTH {
                    error!(
                        "Path exceeded depth filter: {}",
                        url_with_referrer.url.path()
                    );
                    false
                } else {
                    true
                }
            })
            // If internal_only is true, only include URLs that start with the base URL
            .filter(|url_with_referrer| {
                !self.internal_only || url_with_referrer.url.starts_with(&self.base_url)
            })
            .collect()
    }

    async fn check_non_internal_html(&self, url: &Url, referrer: Option<&Url>) {
        match self.lychee_client.check(url.as_str()).await {
            Ok(response) => {
                if !response.status().is_success() {
                    if let Some(ref_url) = referrer {
                        error!(
                            "Link check failed for {} (referrer: {}): {}",
                            url.as_str(),
                            ref_url.as_str(),
                            response.status()
                        );
                    } else {
                        error!(
                            "Link check failed for {}: {}",
                            url.as_str(),
                            response.status()
                        );
                    }
                    self.failed_checks.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.successful_checks.fetch_add(1, Ordering::Relaxed);
                    info!("Successfully checked link: {}", url.as_str());
                }
            }
            Err(e) => {
                if let Some(ref_url) = referrer {
                    error!(
                        "Failed to check link {} (referrer: {}): {}",
                        url.as_str(),
                        ref_url.as_str(),
                        e
                    );
                } else {
                    error!("Failed to check link {}: {}", url.as_str(), e);
                }
                self.failed_checks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn display_summary(&self) {
        let successful_checks = self.successful_checks.load(Ordering::Relaxed);
        let failed_checks = self.failed_checks.load(Ordering::Relaxed);
        let total_checks = successful_checks + failed_checks;

        info!("\nLink Check Summary:");
        info!("Total links checked: {}", total_checks);
        info!("Successful checks: {}", successful_checks);
        info!("Failed checks: {}", failed_checks);
    }

    fn fail_on_error(&self) -> Result<()> {
        if self.failed_checks.load(Ordering::Relaxed) > 0 {
            error!("Some links failed to check");
            anyhow::bail!("Some links failed to check");
        }
        Ok(())
    }
}
