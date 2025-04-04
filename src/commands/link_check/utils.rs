use url::Url;

pub(super) fn is_html(url: &Url, content_type: Option<&str>) -> bool {
    // Check URL extension
    let path = url.path().to_lowercase();
    let is_non_html_extension = path.ends_with(".svg")
        || path.ends_with(".png")
        || path.ends_with(".jpg")
        || path.ends_with(".jpeg")
        || path.ends_with(".gif")
        || path.ends_with(".ico")
        || path.ends_with(".css")
        || path.ends_with(".js")
        || path.ends_with(".json")
        || path.ends_with(".woff")
        || path.ends_with(".woff2")
        || path.ends_with(".ttf")
        || path.ends_with(".eot");
    if is_non_html_extension {
        return false;
    }

    // Check content type if available
    if let Some(content_type) = content_type {
        return content_type.contains("text/html");
    }

    // If no content type is available, assume it might be HTML
    true
}

pub(super) fn normalize_url(url: &Url) -> Url {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized.set_query(None);

    let mut path = normalized.path().to_string();
    if path.ends_with('/') && path.len() > 1 {
        path.pop();
        normalized.set_path(&path);
    }

    normalized
}

pub(super) fn get_origin(url: &Url) -> Option<Url> {
    match url.host_str() {
        Some(host_str) => Url::parse(&format!("{}://{}", url.scheme(), host_str)).ok(),
        None => None,
    }
}

pub(super) trait StartsWith<T> {
    fn starts_with(&self, other: &T) -> bool;
}

impl StartsWith<Url> for Url {
    fn starts_with(&self, base: &Url) -> bool {
        self.origin() == base.origin() && self.path().starts_with(base.path())
    }
}
