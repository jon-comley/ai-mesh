//! One-time Spotify OAuth helper (`just spotify-auth`) — obtains the
//! long-lived refresh token the music capability's Web API control plane
//! runs on, and writes ~/.config/ai-mesh/spotify.env for
//! `just spotify-push-creds <node>` to ship to the music node.
//!
//! WSL2 has no browser, so this is a paste-the-URL flow: the user opens the
//! printed authorize URL on any other device, approves, and pastes the
//! resulting (dead) 127.0.0.1 redirect URL back here. The redirect URI must
//! be a loopback IP literal — Spotify no longer accepts http://localhost.

use std::io::Write;

const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
const SCOPES: &str =
    "user-modify-playback-state user-read-playback-state user-read-currently-playing";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

fn prompt(label: &str) -> String {
    print!("{label}: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    line.trim().to_string()
}

fn env_or_prompt(var: &str, label: &str) -> String {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| prompt(label))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    println!("Spotify one-time authorisation (developer app credentials from");
    println!("developer.spotify.com — see docs/music.md for the walkthrough).");
    println!();
    let client_id = env_or_prompt("SPOTIFY_CLIENT_ID", "Client ID");
    let client_secret = env_or_prompt("SPOTIFY_CLIENT_SECRET", "Client Secret");
    if client_id.is_empty() || client_secret.is_empty() {
        return Err("a Client ID and Client Secret are required".into());
    }

    let authorize_url = reqwest::Url::parse_with_params(
        "https://accounts.spotify.com/authorize",
        &[
            ("client_id", client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
        ],
    )
    .map_err(|e| format!("building authorize URL: {e}"))?;

    println!();
    println!("1. Open this URL in any browser (e.g. on Windows or your phone):");
    println!();
    println!("   {authorize_url}");
    println!();
    println!("2. Log in and approve. The browser will then fail to load a");
    println!("   http://127.0.0.1:8888/callback?code=... page — that's expected.");
    println!("3. Copy the FULL address-bar URL of that dead page and paste it here.");
    println!();
    let pasted = prompt("Redirect URL");
    let code = extract_code(&pasted)?;

    let http = reqwest::Client::new();
    let resp = http
        .post(TOKEN_URL)
        .basic_auth(&client_id, Some(&client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .map_err(|e| format!("could not reach Spotify: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        let detail = body["error_description"]
            .as_str()
            .unwrap_or(status.as_str());
        return Err(format!(
            "token exchange failed: {detail} (the code expires quickly — re-run and paste promptly; \
             the app's Redirect URI must be exactly {REDIRECT_URI})"
        ));
    }
    let Some(refresh_token) = body["refresh_token"].as_str() else {
        return Err("Spotify returned no refresh token".into());
    };

    let path = write_creds_file(&client_id, &client_secret, refresh_token)?;
    println!();
    println!("Success. Credentials written to {}", path.display());
    println!();
    println!("Next: push them to the music node with");
    println!("   just spotify-push-creds pi2");
    Ok(())
}

/// Pull the `code` query param out of the pasted redirect URL; surfaces
/// Spotify's `error` param (e.g. access_denied) if approval was refused.
fn extract_code(pasted: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(pasted.trim())
        .map_err(|_| "that doesn't look like a URL — paste the full address-bar contents")?;
    if let Some((_, e)) = url.query_pairs().find(|(k, _)| k == "error") {
        return Err(format!("Spotify reported '{e}' — approval was not granted"));
    }
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| "no ?code= in that URL — paste the URL from AFTER approving".into())
}

fn write_creds_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<std::path::PathBuf, String> {
    let dir = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".config")
        .join("ai-mesh");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("spotify.env");
    let contents = format!(
        "SPOTIFY_CLIENT_ID={client_id}\nSPOTIFY_CLIENT_SECRET={client_secret}\nSPOTIFY_REFRESH_TOKEN={refresh_token}\n"
    );
    std::fs::write(&path, contents).map_err(|e| format!("writing {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_code_finds_code() {
        let code = extract_code("http://127.0.0.1:8888/callback?code=AQDx12&state=x").unwrap();
        assert_eq!(code, "AQDx12");
    }

    #[test]
    fn extract_code_surfaces_denial() {
        let err = extract_code("http://127.0.0.1:8888/callback?error=access_denied").unwrap_err();
        assert!(err.contains("access_denied"));
    }

    #[test]
    fn extract_code_rejects_non_url() {
        assert!(extract_code("not a url").is_err());
    }
}
