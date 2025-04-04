use std::time::Duration;

use indicatif::ProgressStyle;
use url::Url;

pub(super) struct ProgressBar(indicatif::ProgressBar);

impl ProgressBar {
    pub(super) fn new() -> Self {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .unwrap(),
        );
        ProgressBar(pb)
    }

    pub(super) fn init(&mut self) {
        self.0.set_message("Checking links...");
        self.0.enable_steady_tick(Duration::from_millis(100));
    }

    pub(super) fn finish(&mut self) {
        self.0.finish_and_clear();
    }

    pub(super) fn curr_checking(&mut self, url: &Url) {
        self.0.set_message(format!("Checking: {}", url.as_str()));
    }
}
