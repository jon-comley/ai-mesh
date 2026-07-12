//! Push a phone notification via [ntfy](https://ntfy.sh) — a plain HTTP POST
//! to the configured topic URL, no SDK needed.

/// POST `body` as a notification to `topic_url` (e.g.
/// `https://ntfy.sh/my-private-topic`), with `title` and an optional
/// `click_url` opened when the notification is tapped.
pub async fn send_ntfy(
    topic_url: &str,
    title: &str,
    body: &str,
    click_url: Option<&str>,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut req = client
        .post(topic_url)
        .header("Title", title)
        .body(body.to_string());
    if let Some(url) = click_url {
        req = req.header("Click", url);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("could not reach ntfy: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("ntfy push failed: HTTP {}", resp.status()));
    }
    Ok(())
}
